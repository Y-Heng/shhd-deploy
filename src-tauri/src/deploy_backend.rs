//! 后端部署：校验产物、压缩上传中转、备机同步、滚动替换、健康检查与回滚。

use crate::config::{
    AppConfig, AuthConfig, BackendGroup, BackendProject, CopyMode, ServerConfig,
};
use crate::events::TaskLogger;
use crate::ssh::{self, SshConnection};
use anyhow::{anyhow, bail, Context, Result};
use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// 部署模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeployMode {
    /// 上传到中转并立即替换
    Full,
    /// 只上传到中转目录，稍后再替换
    Stage,
    /// 用已有的中转内容执行替换（不重新上传）
    Replace,
}

fn default_deploy_mode() -> DeployMode {
    DeployMode::Full
}

/// 后端部署请求
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendDeployRequest {
    pub group_id: String,
    pub project_ids: Vec<String>,
    /// 发布名称，如 20260812-优惠券功能
    pub release_name: String,
    /// 覆盖组配置的复制方式
    #[serde(default)]
    pub copy_mode: Option<CopyMode>,
    /// 部署模式
    #[serde(default = "default_deploy_mode")]
    pub mode: DeployMode,
    /// 替换前把应用目录复制为 <目录名>-yyyyMMdd（当天已存在则跳过）
    #[serde(default)]
    pub backup_sibling: bool,
    /// 预览勾选确认后的精确文件列表（相对路径，按项目）。有则只打包这些文件。
    #[serde(default)]
    pub preview_paths: HashMap<String, Vec<String>>,
    /// 本次部署的文件起始日期（YYYY-MM-DD）。有则覆盖项目配置。
    #[serde(default)]
    pub newer_than: Option<String>,
}

/// 发布历史记录（用于回滚）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRecord {
    pub id: String,
    pub release_name: String,
    pub group_id: String,
    pub group_name: String,
    pub project_ids: Vec<String>,
    pub server_ids: Vec<String>,
    pub created_at: String,
    pub status: String,
}

/// 读取发布历史（文件不存在则空列表）
pub fn load_releases() -> Vec<ReleaseRecord> {
    let path = crate::config::releases_file_path();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn persist_releases(records: &[ReleaseRecord]) {
    let _ = std::fs::create_dir_all(crate::config::config_dir());
    if let Ok(content) = serde_json::to_string_pretty(records) {
        let _ = std::fs::write(crate::config::releases_file_path(), content);
    }
}

fn save_release(record: ReleaseRecord) {
    let mut records = load_releases();
    records.insert(0, record);
    // 只保留最近 100 条
    records.truncate(100);
    persist_releases(&records);
}

/// 已中转的发布执行替换成功后，把记录状态改为 success
fn mark_release_success(group_id: &str, release_name: &str) -> bool {
    let mut records = load_releases();
    let mut found = false;
    for record in records.iter_mut() {
        if record.group_id == group_id
            && record.release_name == release_name
            && record.status == "staged"
        {
            record.status = "success".into();
            record.created_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            found = true;
            break;
        }
    }
    if found {
        persist_releases(&records);
    }
    found
}

/// 回滚成功后：原发布标为已回滚，并追加一条回滚记录
fn record_rollback(source: &ReleaseRecord) {
    let mut records = load_releases();
    for record in records.iter_mut() {
        if record.id == source.id && record.status == "success" {
            record.status = "rolled_back".into();
        }
    }
    records.insert(
        0,
        ReleaseRecord {
            id: uuid::Uuid::new_v4().to_string(),
            release_name: format!("回滚 {}", source.release_name),
            group_id: source.group_id.clone(),
            group_name: source.group_name.clone(),
            project_ids: source.project_ids.clone(),
            server_ids: source.server_ids.clone(),
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            status: "rollback".into(),
        },
    );
    records.truncate(100);
    persist_releases(&records);
}

/// 从远程应用目录推导相对站点路径：D:\code\sites\to\service\rest -> to\service\rest
fn relative_site_path(remote_app_dir: &str, fallback_id: &str) -> String {
    let lower = remote_app_dir.to_lowercase();
    if let Some(position) = lower.find("\\sites\\") {
        remote_app_dir[position + "\\sites\\".len()..].to_string()
    } else {
        fallback_id.to_string()
    }
}

/// Windows 路径拼接（统一成反斜杠，避免 SMB 管理共享路径拼错）
fn win_join(base: &str, sub: &str) -> String {
    let base = base.replace('/', "\\");
    let sub = sub.replace('/', "\\");
    format!("{}\\{}", base.trim_end_matches('\\'), sub.trim_start_matches('\\'))
}

fn ps_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

fn wrap_project_scripts(project: &BackendProject, inner_script: &str) -> String {
    let (stop_script, start_script) = crate::service_scripts::resolve_scripts(project);
    crate::service_scripts::wrap_with_service_scripts(&stop_script, &start_script, inner_script)
}

/// 线上实际替换目录：与 remote_app_dir 一比一
fn project_live_dir(project: &BackendProject) -> String {
    project.remote_app_dir.replace('/', "\\")
}

fn project_staging_dir(group: &BackendGroup, release_name: &str, project: &BackendProject) -> String {
    project_pack_dir(&win_join(&group.staging_dir, release_name), project)
}

fn project_backup_dir(group: &BackendGroup, release_name: &str, project: &BackendProject) -> String {
    project_pack_dir(&win_join(&group.backup_dir, release_name), project)
}

/// 中转/备份根目录下该项目的解压位置
fn project_pack_dir(base: &str, project: &BackendProject) -> String {
    let rel_path = relative_site_path(&project.remote_app_dir, &project.id);
    win_join(base, &rel_path)
}

fn parse_newer_than(raw: &Option<String>) -> Result<Option<std::time::SystemTime>> {
    let Some(text) = raw.as_ref().map(|value| value.trim().to_string()).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let naive = if let Ok(date) = chrono::NaiveDate::parse_from_str(&text, "%Y-%m-%d") {
        date.and_hms_opt(0, 0, 0).context("无效日期")?
    } else if let Ok(datetime) = chrono::NaiveDateTime::parse_from_str(&text, "%Y-%m-%d %H:%M:%S") {
        datetime
    } else {
        bail!("文件起始日期格式应为 YYYY-MM-DD，当前: {}", text);
    };
    let local = chrono::Local
        .from_local_datetime(&naive)
        .single()
        .or_else(|| chrono::Local.from_local_datetime(&naive).earliest())
        .with_context(|| format!("无法解析本地时间: {}", text))?;
    Ok(Some(local.into()))
}

fn build_gitignore(root: &Path, rules: &str) -> Result<ignore::gitignore::Gitignore> {
    if rules.trim().is_empty() {
        return Ok(ignore::gitignore::Gitignore::empty());
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    for line in rules.lines() {
        let pattern = line.trim();
        if pattern.is_empty() || pattern.starts_with('#') { continue; }
        builder
            .add_line(None, pattern)
            .with_context(|| format!("忽略规则无效: {}", pattern))?;
    }
    builder.build().context("编译忽略规则失败")
}

fn path_is_ignored(gitignore: &ignore::gitignore::Gitignore, path: &Path, is_dir: bool) -> bool {
    gitignore.matched_path_or_any_parents(path, is_dir).is_ignore()
}

fn file_is_too_old(path: &Path, cutoff: std::time::SystemTime) -> bool {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .map(|modified| modified < cutoff)
        .unwrap_or(false)
}

fn extra_covers(relative: &str, extras: &[String]) -> bool {
    extras.iter().any(|item| {
        let normalized = item.replace('\\', "/").trim_matches('/').to_string();
        if normalized.is_empty() { return false; }
        relative == normalized || relative.starts_with(&format!("{}/", normalized))
    })
}

struct PackFile {
    relative: String,
    included: bool,
    ignored: bool,
    reason: String,
    modified_at: String,
}

/// 忽略规则默认排除；白名单/预览勾选可带上早于日期的文件。忽略文件仍会出现在预览里。
fn classify_project_files(
    project: &BackendProject,
    extra_includes: &[String],
    newer_than_override: Option<&str>,
) -> Result<Vec<PackFile>> {
    let source_dir = Path::new(&project.local_bin_dir);
    if !source_dir.is_dir() {
        bail!("本地产物目录不存在: {}", project.local_bin_dir);
    }
    let ignore = build_gitignore(source_dir, &project.ignore_rules)?;
    let whitelist = build_gitignore(source_dir, &project.whitelist_rules)?;
    let date_raw = match newer_than_override {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => Some(value.to_string()),
        None => project.newer_than.clone(),
    };
    let newer_than = parse_newer_than(&date_raw)?;
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(source_dir) {
        let entry = entry?;
        if !entry.file_type().is_file() { continue; }
        let path = entry.path();
        let relative = path
            .strip_prefix(source_dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if relative.is_empty() { continue; }
        let modified_at = std::fs::metadata(path)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .map(|time| chrono::DateTime::<chrono::Local>::from(time).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let ignored = path_is_ignored(&ignore, path, false);
        if ignored {
            files.push(PackFile {
                relative,
                included: false,
                ignored: true,
                reason: "忽略规则".into(),
                modified_at,
            });
            continue;
        }
        let too_old = newer_than.map(|cutoff| file_is_too_old(path, cutoff)).unwrap_or(false);
        let in_extra = extra_covers(&relative, extra_includes);
        let in_whitelist = path_is_ignored(&whitelist, path, false);
        let (included, reason) = if too_old && (in_extra || in_whitelist) {
            (true, if in_extra { "预览勾选".into() } else { "白名单".into() })
        } else if too_old {
            (false, "早于改动起始日".into())
        } else {
            (true, String::new())
        };
        files.push(PackFile { relative, included, ignored: false, reason, modified_at });
    }
    files.sort_by(|left, right| right.modified_at.cmp(&left.modified_at));
    Ok(files)
}

/// 打包预览树节点：是否纳入、是否被忽略、修改时间
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackTreeNode {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub included: bool,
    pub ignored: bool,
    pub disabled: bool,
    pub reason: String,
    pub modified_at: Option<String>,
    pub children: Vec<PackTreeNode>,
}

/// 单个项目的打包预览（树 + 计数）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPackPreview {
    pub project_id: String,
    pub project_name: String,
    pub local_dir: String,
    pub included_count: u64,
    pub old_count: u64,
    pub ignored_count: u64,
    pub tree: Vec<PackTreeNode>,
}

#[derive(Default)]
struct TreeBuilder {
    dirs: BTreeMap<String, TreeBuilder>,
    file: Option<PackFile>,
}

fn build_pack_tree(files: &[PackFile]) -> Vec<PackTreeNode> {
    let mut root = TreeBuilder::default();
    for file in files {
        let mut current = &mut root;
        let parts: Vec<&str> = file.relative.split('/').filter(|part| !part.is_empty()).collect();
        for (index, part) in parts.iter().enumerate() {
            if index + 1 == parts.len() {
                current.dirs.entry((*part).to_string()).or_default().file = Some(PackFile {
                    relative: file.relative.clone(),
                    included: file.included,
                    ignored: file.ignored,
                    reason: file.reason.clone(),
                    modified_at: file.modified_at.clone(),
                });
            } else {
                current = current.dirs.entry((*part).to_string()).or_default();
            }
        }
    }
    flatten_tree("", &root)
}

fn flatten_tree(parent: &str, builder: &TreeBuilder) -> Vec<PackTreeNode> {
    let mut nodes: Vec<PackTreeNode> = builder
        .dirs
        .iter()
        .map(|(name, child)| {
            let path = if parent.is_empty() { name.clone() } else { format!("{}/{}", parent, name) };
            if let Some(file) = &child.file {
                PackTreeNode {
                    path,
                    name: name.clone(),
                    is_dir: false,
                    included: file.included,
                    ignored: file.ignored,
                    disabled: false,
                    reason: file.reason.clone(),
                    modified_at: Some(file.modified_at.clone()).filter(|value| !value.is_empty()),
                    children: Vec::new(),
                }
            } else {
                let children = flatten_tree(&path, child);
                let included = children.iter().any(|node| node.included);
                let ignored = !children.is_empty() && children.iter().all(|node| node.ignored);
                PackTreeNode {
                    path,
                    name: name.clone(),
                    is_dir: true,
                    included,
                    ignored,
                    disabled: false,
                    reason: if ignored { "忽略规则".into() } else { String::new() },
                    modified_at: node_newest_time(&children),
                    children,
                }
            }
        })
        .collect();
    sort_pack_nodes(&mut nodes);
    nodes
}

fn node_newest_time(nodes: &[PackTreeNode]) -> Option<String> {
    nodes
        .iter()
        .filter_map(|node| node.modified_at.clone())
        .max()
}

fn sort_pack_nodes(nodes: &mut [PackTreeNode]) {
    nodes.sort_by(|left, right| match left.ignored.cmp(&right.ignored) {
        std::cmp::Ordering::Equal => right
            .modified_at
            .as_deref()
            .unwrap_or("")
            .cmp(left.modified_at.as_deref().unwrap_or("")),
        order => order,
    });
}

/// 部署预览：忽略文件会显示在末尾且默认不勾选；早于改动起始日的默认不勾选
/// 按忽略规则、白名单和改动起始日生成打包预览树
pub fn preview_backend_pack(
    config: &AppConfig,
    group_id: &str,
    project_ids: &[String],
    newer_than: Option<&str>,
) -> Result<Vec<ProjectPackPreview>> {
    let group = config
        .backend_groups
        .iter()
        .find(|candidate| candidate.id == group_id)
        .with_context(|| format!("找不到负载组: {}", group_id))?;
    let mut previews = Vec::new();
    for project in &group.projects {
        if !project_ids.contains(&project.id) { continue; }
        let files = classify_project_files(project, &[], newer_than)?;
        let included_count = files.iter().filter(|file| file.included).count() as u64;
        let ignored_count = files.iter().filter(|file| file.ignored).count() as u64;
        let old_count = files.iter().filter(|file| !file.included && !file.ignored).count() as u64;
        previews.push(ProjectPackPreview {
            project_id: project.id.clone(),
            project_name: project.name.clone(),
            local_dir: project.local_bin_dir.clone(),
            included_count,
            old_count,
            ignored_count,
            tree: build_pack_tree(&files),
        });
    }
    if previews.is_empty() { bail!("未选择任何项目"); }
    Ok(previews)
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() { bail!("任务已被取消"); }
    Ok(())
}

/// 按忽略规则、时间、白名单和预览勾选压缩产物
fn zip_project_directory(
    project: &BackendProject,
    zip_path: &Path,
    extra_includes: &[String],
    only_paths: Option<&HashSet<String>>,
    newer_than_override: Option<&str>,
) -> Result<(u64, u64, u64)> {
    use std::io::{Read, Write};
    use zip::write::SimpleFileOptions;

    let source_dir = Path::new(&project.local_bin_dir);
    let files = classify_project_files(project, extra_includes, newer_than_override)?;
    let zip_file = std::fs::File::create(zip_path)
        .with_context(|| format!("创建压缩包失败: {}", zip_path.display()))?;
    let mut zip_writer = zip::ZipWriter::new(zip_file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);

    let mut file_count: u64 = 0;
    let mut skipped_count: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut read_buffer = vec![0u8; 1024 * 1024];

    for file in files {
        let pack = match only_paths {
            Some(paths) => paths.contains(&file.relative),
            None => file.included,
        };
        if !pack {
            skipped_count += 1;
            continue;
        }
        let mut abs_path = source_dir.to_path_buf();
        for part in file.relative.split('/') {
            abs_path.push(part);
        }
        zip_writer.start_file(&file.relative, options)?;
        let mut source_file = std::fs::File::open(&abs_path)?;
        loop {
            let read = source_file.read(&mut read_buffer)?;
            if read == 0 { break; }
            zip_writer.write_all(&read_buffer[..read])?;
            total_bytes += read as u64;
        }
        file_count += 1;
    }
    zip_writer.finish()?;
    if file_count == 0 {
        bail!(
            "{} 过滤后没有可部署文件（请检查忽略规则、文件起始日期、白名单或预览勾选）",
            project.name
        );
    }
    Ok((file_count, skipped_count, total_bytes))
}

/// SFTP 上传单个文件，带进度回调
async fn upload_file(
    conn: &SshConnection,
    local_path: &Path,
    remote_path: &str,
    logger: &TaskLogger,
    progress_base: f64,
    progress_span: f64,
    step_name: &str,
) -> Result<()> {
    let remote_sftp_path = ssh::to_sftp_path(remote_path);
    let mut local_file = tokio::fs::File::open(local_path)
        .await
        .with_context(|| format!("打开本地文件失败: {}", local_path.display()))?;
    let total_bytes = local_file.metadata().await?.len();
    let started = Instant::now();
    let mut last_error = None;

    logger.info(format!(
        "上传路径: {}，大小 {:.2} MB",
        ssh::describe_connection(conn),
        total_bytes as f64 / 1024.0 / 1024.0
    ));

    if conn.jump_server.is_some() {
        logger.info(format!(
            "先传到跳板机，再内网拷到 {}（不走 Windows SFTP 公网逐包确认）",
            conn.server.name
        ));
        let mut last_report = Instant::now();
        match ssh::upload_through_jump(conn, local_path, remote_path, |sent, total| {
            if last_report.elapsed().as_millis() > 500 {
                let fraction = sent as f64 / total.max(1) as f64;
                let speed = sent as f64 / 1024.0 / 1024.0 / started.elapsed().as_secs_f64().max(0.001);
                logger.progress(
                    progress_base + progress_span * fraction,
                    format!("{} {:.1}% ({:.2} MB/s)", step_name, fraction * 100.0, speed),
                );
                last_report = Instant::now();
            }
        })
        .await
        {
            Ok(()) => {
                let elapsed = started.elapsed().as_secs_f64();
                logger.info(format!(
                    "上传完成: {:.2} MB，耗时 {:.1} 秒（{:.2} MB/s）",
                    total_bytes as f64 / 1024.0 / 1024.0,
                    elapsed,
                    total_bytes as f64 / 1024.0 / 1024.0 / elapsed.max(0.001)
                ));
                return Ok(());
            }
            Err(error) => {
                logger.warn(format!("跳板机中转失败，改走 Windows SFTP: {:#}", error));
                local_file
                    .seek(std::io::SeekFrom::Start(0))
                    .await
                    .context("回退直传前重置本地文件指针失败")?;
            }
        }
    }

    for attempt in 0..2 {
        if attempt > 0 {
            logger.info(format!(
                "上传中断，正在重试: {:#}",
                last_error.as_ref().unwrap()
            ));
            local_file
                .seek(std::io::SeekFrom::Start(0))
                .await
                .context("重试上传时重置本地文件指针失败")?;
        }

        let sftp = ssh::open_sftp(conn).await?;
        if let Some(slash_pos) = remote_sftp_path.rfind('/') { ssh::sftp_mkdir_all(&sftp, &remote_sftp_path[..slash_pos]).await?; }

        match upload_file_once(
            &sftp,
            &mut local_file,
            &remote_sftp_path,
            total_bytes,
            logger,
            progress_base,
            progress_span,
            step_name,
            ssh::sftp_write_chunk(conn.server.os),
        )
        .await
        {
            Ok(()) => {
                last_error = None;
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    if let Some(error) = last_error { return Err(error); }

    let elapsed = started.elapsed().as_secs_f64();
    logger.info(format!(
        "上传完成: {:.2} MB，耗时 {:.1} 秒（{:.2} MB/s）",
        total_bytes as f64 / 1024.0 / 1024.0,
        elapsed,
        total_bytes as f64 / 1024.0 / 1024.0 / elapsed.max(0.001)
    ));
    Ok(())
}

/// 单次 SFTP 写入（失败时由上层重试）
async fn upload_file_once(
    sftp: &russh_sftp::client::SftpSession,
    local_file: &mut tokio::fs::File,
    remote_sftp_path: &str,
    total_bytes: u64,
    logger: &TaskLogger,
    progress_base: f64,
    progress_span: f64,
    step_name: &str,
    chunk_size: usize,
) -> Result<()> {
    let mut remote_file = sftp
        .create(remote_sftp_path.to_string())
        .await
        .with_context(|| format!("创建远端文件失败: {}", remote_sftp_path))?;

    let mut buffer = vec![0u8; chunk_size];
    let mut sent_bytes: u64 = 0;
    let started = Instant::now();
    let mut last_report = Instant::now();
    let progress_total = total_bytes.max(1);

    loop {
        let read = local_file.read(&mut buffer).await?;
        if read == 0 { break; }
        remote_file
            .write_all(&buffer[..read])
            .await
            .with_context(|| {
                format!(
                    "写入远端失败（已上传 {:.2} MB / {:.2} MB）: {}",
                    sent_bytes as f64 / 1024.0 / 1024.0,
                    total_bytes as f64 / 1024.0 / 1024.0,
                    remote_sftp_path
                )
            })?;
        sent_bytes += read as u64;
        if last_report.elapsed().as_millis() > 500 {
            let fraction = sent_bytes as f64 / progress_total as f64;
            let speed_mbps =
                sent_bytes as f64 / 1024.0 / 1024.0 / started.elapsed().as_secs_f64().max(0.001);
            logger.progress(
                progress_base + progress_span * fraction,
                format!(
                    "{} {:.1}% ({:.2} MB/s)",
                    step_name,
                    fraction * 100.0,
                    speed_mbps
                ),
            );
            last_report = Instant::now();
        }
    }
    remote_file
        .shutdown()
        .await
        .with_context(|| {
            format!(
                "关闭远端文件失败（已上传 {:.2} MB / {:.2} MB）: {}",
                sent_bytes as f64 / 1024.0 / 1024.0,
                total_bytes as f64 / 1024.0 / 1024.0,
                remote_sftp_path
            )
        })?;
    Ok(())
}

/// 在 Windows 服务器上执行 PowerShell 脚本并要求成功
async fn run_ps(conn: &SshConnection, script: &str, logger: &TaskLogger) -> Result<String> {
    let command = ssh::powershell_command(script);
    let mut line_callback = |line: &str| {
        if line.trim().is_empty() || ssh::is_clixml_noise(line) { return; }
        logger.info(format!("  [{}] {}", conn.server.name, line));
    };
    let output = ssh::exec(conn, &command, Some(&mut line_callback)).await?;
    if !output.success() {
        bail!(
            "服务器 {} 执行脚本失败(退出码 {}): {}",
            conn.server.name,
            output.exit_code,
            output.combined().chars().take(2000).collect::<String>()
        );
    }
    Ok(output.stdout)
}

/// 健康检查：状态码 < 500 视为服务存活
async fn health_check(
    conn: &SshConnection,
    project: &BackendProject,
    logger: &TaskLogger,
) -> Result<()> {
    let Some(url) = &project.health_check_url else {
        logger.warn(format!("项目 {} 未配置健康检查地址，跳过", project.name));
        return Ok(());
    };
    let escaped_url = url.replace('\'', "''");
    let script = format!(
        "try {{ Invoke-WebRequest -Uri '{}' -UseBasicParsing -TimeoutSec 10 | Out-Null; exit 0 }} catch {{ $statusCode = 0; try {{ $statusCode = [int]$_.Exception.Response.StatusCode }} catch {{}}; if ($statusCode -gt 0 -and $statusCode -lt 500) {{ exit 0 }} else {{ exit 1 }} }}",
        escaped_url
    );
    let command = ssh::powershell_command(&script);

    for attempt in 1..=project.health_check_retries.max(1) {
        let output = ssh::exec(conn, &command, None).await?;
        if output.success() {
            logger.success(format!(
                "健康检查通过: {} @ {} (第 {} 次)",
                project.name, conn.server.name, attempt
            ));
            return Ok(());
        }
        logger.warn(format!(
            "健康检查未通过: {} @ {} (第 {}/{} 次)，{} 秒后重试",
            project.name,
            conn.server.name,
            attempt,
            project.health_check_retries,
            project.health_check_delay_secs
        ));
        tokio::time::sleep(std::time::Duration::from_secs(
            project.health_check_delay_secs as u64,
        ))
        .await;
    }
    bail!(
        "项目 {} 在服务器 {} 健康检查失败",
        project.name,
        conn.server.name
    );
}

/// 在单台服务器上完成"停 IIS → 备份 → 替换 → 启动 IIS → 健康检查"
async fn deploy_to_server(
    conn: &SshConnection,
    group: &BackendGroup,
    project: &BackendProject,
    release_name: &str,
    backup_sibling: bool,
    date_suffix: &str,
    logger: &TaskLogger,
) -> Result<()> {
    let live_dir = project_live_dir(project);
    let staging_dir = project_staging_dir(group, release_name, project);
    let backup_dir = project_backup_dir(group, release_name, project);
    let sibling_dir = format!(
        "{}-{}",
        project.remote_app_dir.trim_end_matches('\\'),
        date_suffix
    );

    logger.info(format!(
        "[{}] 部署 {}: 覆盖 {}（不删除线上其它文件）",
        conn.server.name, project.name, live_dir
    ));

    let sibling_block = if backup_sibling {
        format!(
            r#"
if ($actionExit -eq 0) {{
  $targetDir = '{target_dir}'
  $siblingDir = '{sibling_dir}'
  if (-not (Test-Path -LiteralPath $targetDir)) {{ Write-Output '目录不存在，跳过日期备份' }}
  elseif (Test-Path -LiteralPath $siblingDir) {{ Write-Output ('今日备份已存在(' + $siblingDir + ')，跳过') }}
  else {{
    robocopy $targetDir $siblingDir /E /R:2 /W:3 /NP /NFL /NDL | Out-Null
    if ($LASTEXITCODE -ge 8) {{ Write-Output '日期备份失败'; $actionExit = 2 }}
    else {{ Write-Output ('目录已备份 -> ' + $siblingDir) }}
  }}
}}
"#,
            target_dir = project.remote_app_dir.replace('\'', "''"),
            sibling_dir = sibling_dir.replace('\'', "''"),
        )
    } else {
        String::new()
    };

    let inner = format!(
        r#"{sibling_block}
if ($actionExit -eq 0) {{
  if (-not (Test-Path -LiteralPath '{staging_dir}')) {{ Write-Output '暂存目录不存在'; $actionExit = 4 }}
  else {{
    if (Test-Path -LiteralPath '{live_dir}') {{
      robocopy '{live_dir}' '{backup_dir}' /E /R:2 /W:3 /NP /NFL /NDL | Out-Null
      if ($LASTEXITCODE -ge 8) {{ Write-Output '备份失败'; $actionExit = 2 }}
      else {{ Write-Output ('备份完成 -> {backup_dir}') }}
    }} else {{
      Write-Output '目标目录不存在，跳过备份（首次部署）'
    }}
    if ($actionExit -eq 0) {{
      robocopy '{staging_dir}' '{live_dir}' /E /R:5 /W:2 /NP /NFL /NDL | Out-Null
      if ($LASTEXITCODE -ge 8) {{ Write-Output '替换失败'; $actionExit = 3 }}
      else {{ Write-Output '覆盖完成（未删除线上其它文件）' }}
    }}
  }}
}}
"#,
        sibling_block = sibling_block,
        staging_dir = staging_dir.replace('\'', "''"),
        live_dir = live_dir.replace('\'', "''"),
        backup_dir = backup_dir.replace('\'', "''"),
    );
    let script = wrap_project_scripts(project, &inner);
    let (stop_script, _) = crate::service_scripts::resolve_scripts(project);
    if !stop_script.trim().is_empty() {
        logger.info(format!(
            "[{}] 替换前先停本项目关联服务，完成后再启动",
            conn.server.name
        ));
    }
    run_ps(conn, &script, logger).await?;
    health_check(conn, project, logger).await?;
    Ok(())
}

/// 首台服务器通过 SMB 管理共享把整个发布目录复制到组内其他服务器
async fn smb_copy_to_target(
    source_conn: &SshConnection,
    target: &ServerConfig,
    group: &BackendGroup,
    release_name: &str,
    logger: &TaskLogger,
) -> Result<()> {
    let secondary = target;
    let AuthConfig::Password { password } = &secondary.auth else {
        bail!("服务器 {} 使用密钥认证，无法走 SMB 复制，请把该组复制方式改为 upload", secondary.name);
    };
    let staging_release = win_join(&group.staging_dir, release_name);
    let drive_letter = group
        .staging_dir
        .replace('/', "\\")
        .chars()
        .next()
        .ok_or_else(|| anyhow!("暂存目录配置为空"))?;
    if !drive_letter.is_ascii_alphabetic() { bail!("暂存目录必须是带盘符的 Windows 路径（如 D:\\code\\sites\\devlop），当前: {}", group.staging_dir); }
    let path_after_drive = staging_release
        .get(2..)
        .unwrap_or("")
        .trim_start_matches('\\');
    if path_after_drive.is_empty() { bail!("暂存目录无效，无法拼出管理共享路径: {}", group.staging_dir); }
    let share_root = format!("\\\\{}\\{}$", secondary.host, drive_letter);
    let remote_share_path = format!("{}\\{}", share_root, path_after_drive);
    let configured_user = ps_single_quote(&secondary.username);
    let host_user = if secondary.username.contains('\\') {
        configured_user.clone()
    } else {
        ps_single_quote(&format!("{}\\{}", secondary.host, secondary.username))
    };

    logger.info(format!(
        "[{}] 通过内网 SMB 复制发布目录到 {}（{} -> {}）...",
        source_conn.server.name, secondary.name, staging_release, remote_share_path
    ));

    let script = format!(
        r#"$ErrorActionPreference = 'Continue'
$shareRoot = '{share_root}'
$password = '{password}'
$src = '{staging_release}'
$dst = '{remote_share_path}'
$users = @('{user}')
if ('{host_user}' -ne '{user}') {{ $users += '{host_user}' }}

net use $shareRoot /delete /y 2>$null | Out-Null
$mapped = $false
foreach ($userName in $users) {{
  if ([string]::IsNullOrWhiteSpace($userName)) {{ continue }}
  Write-Output ('尝试映射 ' + $shareRoot + ' ，用户 ' + $userName)
  net use $shareRoot $password /user:$userName /persistent:no
  if ($LASTEXITCODE -eq 0) {{ $mapped = $true; break }}
  Write-Output ('net use 退出码 ' + $LASTEXITCODE)
}}
if (-not $mapped) {{
  Write-Output ('SMB 映射失败，无法访问 ' + $shareRoot)
  Write-Output '请检查：1) 主服务器能否访问备机管理共享 2) 账号密码是否正确 3) 备机已开启文件共享且 D$ 等管理共享可用 4) 本机账号访问管理共享需在备机设置 LocalAccountTokenFilterPolicy=1'
  exit 1
}}
if (-not (Test-Path -LiteralPath $src)) {{
  Write-Output ('源目录不存在: ' + $src)
  net use $shareRoot /delete /y 2>$null | Out-Null
  exit 1
}}
$dstParent = Split-Path -Parent $dst
if (-not (Test-Path -LiteralPath $dstParent)) {{
  New-Item -ItemType Directory -Path $dstParent -Force | Out-Null
}}
robocopy $src $dst /E /R:2 /W:5 /NP /NFL /NDL
$copyResult = $LASTEXITCODE
net use $shareRoot /delete /y 2>$null | Out-Null
if ($copyResult -ge 8) {{
  Write-Output ('SMB 复制失败，robocopy 退出码 ' + $copyResult + '（源: ' + $src + ' -> 目标: ' + $dst + '）')
  if ($copyResult -eq 16) {{ Write-Output '退出码 16 表示严重错误：路径无效、无权限或共享不可用' }}
  exit 1
}}
Write-Output '内网复制完成'
exit 0"#,
        share_root = ps_single_quote(&share_root),
        password = ps_single_quote(password),
        user = configured_user,
        host_user = host_user,
        staging_release = ps_single_quote(&staging_release),
        remote_share_path = ps_single_quote(&remote_share_path),
    );

    run_ps(source_conn, &script, logger).await?;
    Ok(())
}

/// 把单个项目的产物上传到某台服务器并解压到中转目录
fn remote_upload_zip(staging_release: &str, project: &BackendProject) -> String {
    win_join(
        &win_join(staging_release, "_upload"),
        &format!("{}.zip", project.id),
    )
}

async fn expand_uploaded_zip(
    conn: &SshConnection,
    project: &BackendProject,
    staging_release: &str,
    logger: &TaskLogger,
) -> Result<()> {
    let remote_zip = remote_upload_zip(staging_release, project);
    let staging_dir = project_pack_dir(staging_release, project);
    let expand_script = format!(
        r#"$ErrorActionPreference = 'Stop'
if (Test-Path '{staging_dir}') {{ Remove-Item -LiteralPath '{staging_dir}' -Recurse -Force }}
Expand-Archive -LiteralPath '{remote_zip}' -DestinationPath '{staging_dir}' -Force
Write-Output '解压完成 -> {staging_dir}'"#,
        staging_dir = staging_dir.replace('\'', "''"),
        remote_zip = remote_zip.replace('\'', "''"),
    );
    run_ps(conn, &expand_script, logger).await?;
    Ok(())
}

async fn upload_and_expand(
    conn: &SshConnection,
    project: &BackendProject,
    zip_path: &Path,
    staging_release: &str,
    logger: &TaskLogger,
    progress_base: f64,
    progress_span: f64,
    label: &str,
) -> Result<()> {
    let remote_zip = remote_upload_zip(staging_release, project);
    upload_file(
        conn,
        zip_path,
        &remote_zip,
        logger,
        progress_base,
        progress_span,
        label,
    )
    .await?;
    expand_uploaded_zip(conn, project, staging_release, logger).await?;
    Ok(())
}

/// 后端部署主流程
pub async fn run_backend_deploy(
    config: AppConfig,
    request: BackendDeployRequest,
    logger: TaskLogger,
    cancel: CancellationToken,
) -> Result<()> {
    let group = config
        .backend_groups
        .iter()
        .find(|candidate| candidate.id == request.group_id)
        .with_context(|| format!("找不到负载组: {}", request.group_id))?
        .clone();

    let projects: Vec<BackendProject> = group
        .projects
        .iter()
        .filter(|project| request.project_ids.contains(&project.id))
        .cloned()
        .collect();
    if projects.is_empty() {
        bail!("未选择任何项目");
    }
    if request.release_name.trim().is_empty() {
        bail!("发布名称不能为空");
    }
    let release_name = request.release_name.trim().to_string();
    let copy_mode = request.copy_mode.unwrap_or(group.copy_mode);

    // 解析组内服务器列表（第一台作为上传中转与滚动起点）
    let server_ids = group.effective_server_ids();
    if server_ids.is_empty() {
        bail!("负载组 {} 未配置任何服务器", group.name);
    }
    let servers: Vec<ServerConfig> = server_ids
        .iter()
        .map(|server_id| config.find_server(server_id).map(|server| server.clone()))
        .collect::<Result<Vec<_>>>()?;
    let first_server = servers[0].clone();

    let mode_text = match request.mode {
        DeployMode::Full => "上传并替换",
        DeployMode::Stage => "仅上传到中转",
        DeployMode::Replace => "从中转替换",
    };
    logger.state("running", format!("发布 {}", release_name));
    logger.info(format!(
        "开始部署({}): {} | 组: {}（{} 台服务器）| 项目: {}",
        mode_text,
        release_name,
        group.name,
        servers.len(),
        projects
            .iter()
            .map(|project| project.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // 从中转替换模式：跳过本地校验、压缩、上传，直接进入替换阶段
    if request.mode == DeployMode::Replace {
        return replace_phase(
            &config,
            &group,
            &projects,
            &release_name,
            &servers,
            None,
            request.mode,
            request.backup_sibling,
            &logger,
            &cancel,
        )
        .await;
    }

    // 第 1 步：本地产物校验
    logger.progress(2.0, "校验本地产物");
    for project in &projects {
        let bin_path = Path::new(&project.local_bin_dir);
        if !bin_path.is_dir() {
            bail!("本地产物目录不存在: {}", project.local_bin_dir);
        }
        let mut newest: Option<std::time::SystemTime> = None;
        let mut file_count = 0u64;
        for entry in walkdir::WalkDir::new(bin_path).into_iter().flatten() {
            if entry.file_type().is_file() {
                file_count += 1;
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if newest.map_or(true, |current| modified > current) {
                            newest = Some(modified);
                        }
                    }
                }
            }
        }
        if file_count == 0 {
            bail!("本地产物目录为空: {}", project.local_bin_dir);
        }
        let extra = request
            .preview_paths
            .get(&project.id)
            .cloned()
            .unwrap_or_default();
        let files = classify_project_files(project, &extra, request.newer_than.as_deref())?;
        let included = if let Some(paths) = request.preview_paths.get(&project.id) {
            let selected: HashSet<String> = paths.iter().cloned().collect();
            files.iter().filter(|file| selected.contains(&file.relative)).count() as u64
        } else {
            files.iter().filter(|file| file.included).count() as u64
        };
        if included == 0 {
            bail!(
                "{} 按忽略规则/日期/白名单/预览过滤后没有可部署文件（目录共 {} 个文件）",
                project.name,
                file_count
            );
        }
        if request.preview_paths.contains_key(&project.id) {
            logger.info(format!("{}: 按预览勾选打包 {} 个文件", project.name, included));
        }
        if let Some(cutoff) = parse_newer_than(
            &match request.newer_than.as_deref() {
                Some(value) if value.trim().is_empty() => None,
                Some(value) => Some(value.to_string()),
                None => project.newer_than.clone(),
            },
        )? {
            logger.info(format!(
                "{}: 只上传 {} 及之后改过的文件（白名单/预览勾选除外），当前收录 {} / {} 个",
                project.name,
                chrono::DateTime::<chrono::Local>::from(cutoff).format("%Y-%m-%d"),
                included,
                file_count
            ));
        }
        if !project.ignore_rules.trim().is_empty() || !project.whitelist_rules.trim().is_empty() {
            logger.info(format!(
                "{}: 忽略/白名单已启用，收录 {} / {} 个文件",
                project.name, included, file_count
            ));
        }
        if let Some(newest_time) = newest {
            let age = std::time::SystemTime::now()
                .duration_since(newest_time)
                .unwrap_or_default();
            let age_minutes = age.as_secs() / 60;
            let newest_text = chrono::DateTime::<chrono::Local>::from(newest_time)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
            if age_minutes > 24 * 60 {
                logger.warn(format!(
                    "注意: {} 产物最新文件时间 {}（{} 小时前），请确认不是旧包！",
                    project.name,
                    newest_text,
                    age_minutes / 60
                ));
            } else {
                logger.info(format!(
                    "{}: {} 个文件，最新文件时间 {}",
                    project.name, file_count, newest_text
                ));
            }
        }
    }
    check_cancel(&cancel)?;

    // 第 2 步：本地压缩
    logger.progress(5.0, "压缩产物");
    let temp_dir = std::env::temp_dir()
        .join("shhd-deploy")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&temp_dir)?;
    let mut zip_paths: Vec<(BackendProject, PathBuf)> = Vec::new();
    for project in &projects {
        let zip_path = temp_dir.join(format!("{}.zip", project.id));
        let zip_path_clone = zip_path.clone();
        let project_for_zip = project.clone();
        let extra = request
            .preview_paths
            .get(&project.id)
            .cloned()
            .unwrap_or_default();
        let only_paths: Option<HashSet<String>> = request
            .preview_paths
            .get(&project.id)
            .map(|paths| paths.iter().cloned().collect());
        let newer_than = request.newer_than.clone();
        let (file_count, skipped_count, total_bytes) = tokio::task::spawn_blocking(move || {
            zip_project_directory(
                &project_for_zip,
                &zip_path_clone,
                &extra,
                only_paths.as_ref(),
                newer_than.as_deref(),
            )
        })
        .await??;
        let zip_size = std::fs::metadata(&zip_path)?.len();
        logger.info(format!(
            "压缩 {}: 收录 {} 个文件（跳过 {} 个），{:.2} MB -> {:.2} MB",
            project.name,
            file_count,
            skipped_count,
            total_bytes as f64 / 1024.0 / 1024.0,
            zip_size as f64 / 1024.0 / 1024.0
        ));
        zip_paths.push((project.clone(), zip_path));
    }
    check_cancel(&cancel)?;

    // 第 3 步：连接首台服务器并上传中转
    logger.progress(10.0, format!("连接服务器 {}", first_server.name));
    logger.info(ssh::describe_route(&config, &first_server));
    let first_conn = ssh::connect(&config, &first_server.id).await?;
    logger.success(format!("已连接 {}（{}）", first_server.name, ssh::describe_connection(&first_conn)));

    let staging_release = win_join(&group.staging_dir, &release_name);
    let cleanup_script = format!(
        r#"$uploadDir = '{}'
if (Test-Path $uploadDir) {{ Remove-Item -LiteralPath $uploadDir -Recurse -Force }}"#,
        win_join(&staging_release, "_upload")
    );
    let other_servers = &servers[1..];

    if copy_mode == CopyMode::Upload && first_conn.jump_server.is_some() && !other_servers.is_empty() {
        logger.info("采用 SSH 分发 zip：公网上传到跳板机一次，再内网拷到组内各 Windows，无需 D$");
        let upload_total = zip_paths.len() as f64;
        for (index, (project, zip_path)) in zip_paths.iter().enumerate() {
            check_cancel(&cancel)?;
            let progress_base = 12.0 + 42.0 * (index as f64 / upload_total);
            let progress_span = 20.0 / upload_total;
            logger.progress(progress_base, format!("上传 {} 到跳板机", project.name));
            let started = Instant::now();
            let mut last_report = Instant::now();
            let staging = ssh::stage_on_jump(&first_conn, zip_path, |sent, total| {
                if last_report.elapsed().as_millis() > 500 {
                    let fraction = sent as f64 / total.max(1) as f64;
                    let speed = sent as f64 / 1024.0 / 1024.0 / started.elapsed().as_secs_f64().max(0.001);
                    logger.progress(
                        progress_base + progress_span * fraction,
                        format!("上传 {} 到跳板机 {:.1}% ({:.2} MB/s)", project.name, fraction * 100.0, speed),
                    );
                    last_report = Instant::now();
                }
            })
            .await;
            let staging = match staging {
                Ok(value) => value,
                Err(error) => {
                    logger.warn(format!("跳板机中转失败，改为逐台上传: {:#}", error));
                    upload_and_expand(
                        &first_conn,
                        project,
                        zip_path,
                        &staging_release,
                        &logger,
                        progress_base,
                        progress_span,
                        &format!("上传 {}", project.name),
                    )
                    .await?;
                    for target in other_servers {
                        check_cancel(&cancel)?;
                        let conn = ssh::connect(&config, &target.id).await?;
                        upload_and_expand(
                            &conn,
                            project,
                            zip_path,
                            &staging_release,
                            &logger,
                            progress_base + progress_span,
                            0.0,
                            &format!("上传 {} → {}", project.name, target.name),
                        )
                        .await?;
                        let _ = run_ps(&conn, &cleanup_script, &logger).await;
                    }
                    continue;
                }
            };

            let remote_zip = remote_upload_zip(&staging_release, project);
            let fanout = async {
                logger.info(format!("内网分发 {} -> {}", project.name, first_server.name));
                ssh::copy_jump_payload_to_windows(&first_conn, &staging, &first_conn, &remote_zip).await?;
                expand_uploaded_zip(&first_conn, project, &staging_release, &logger).await?;
                for target in other_servers {
                    check_cancel(&cancel)?;
                    logger.progress(
                        progress_base + progress_span + 10.0,
                        format!("内网分发 {} -> {}", project.name, target.name),
                    );
                    let conn = ssh::connect(&config, &target.id).await?;
                    logger.info(ssh::describe_connection(&conn));
                    ssh::copy_jump_payload_to_windows(&first_conn, &staging, &conn, &remote_zip).await?;
                    expand_uploaded_zip(&conn, project, &staging_release, &logger).await?;
                    let _ = run_ps(&conn, &cleanup_script, &logger).await;
                    logger.success(format!("{} 已同步到 {}", project.name, target.name));
                }
                Ok::<(), anyhow::Error>(())
            }
            .await;
            ssh::cleanup_jump_payload(&first_conn, &staging).await;
            fanout?;
        }
        let _ = run_ps(&first_conn, &cleanup_script, &logger).await;
    } else {
        let upload_total = zip_paths.len() as f64;
        for (index, (project, zip_path)) in zip_paths.iter().enumerate() {
            check_cancel(&cancel)?;
            let progress_base = 12.0 + 30.0 * (index as f64 / upload_total);
            let progress_span = 30.0 / upload_total;
            logger.info(format!("上传 {} 到 {} ...", project.name, first_server.name));
            upload_and_expand(
                &first_conn,
                project,
                zip_path,
                &staging_release,
                &logger,
                progress_base,
                progress_span,
                &format!("上传 {}", project.name),
            )
            .await?;
        }
        let _ = run_ps(&first_conn, &cleanup_script, &logger).await;
        check_cancel(&cancel)?;

        for (server_index, target) in other_servers.iter().enumerate() {
            check_cancel(&cancel)?;
            let progress = 45.0 + 12.0 * (server_index as f64 / other_servers.len().max(1) as f64);
            logger.progress(progress, format!("同步发布目录到 {}", target.name));
            match copy_mode {
                CopyMode::Smb => {
                    smb_copy_to_target(&first_conn, target, &group, &release_name, &logger).await?;
                }
                CopyMode::Upload => {
                    logger.info(format!("连接 {} 并直接上传 ...", target.name));
                    let conn = ssh::connect(&config, &target.id).await?;
                    for (project, zip_path) in &zip_paths {
                        check_cancel(&cancel)?;
                        upload_and_expand(
                            &conn,
                            project,
                            zip_path,
                            &staging_release,
                            &logger,
                            progress,
                            0.0,
                            &format!("上传 {} → {}", project.name, target.name),
                        )
                        .await?;
                    }
                    let _ = run_ps(&conn, &cleanup_script, &logger).await;
                }
            }
        }
    }
    check_cancel(&cancel)?;

    // 清理本地临时压缩包
    let _ = std::fs::remove_dir_all(&temp_dir);

    // 仅中转模式：记录待替换发布后结束
    if request.mode == DeployMode::Stage {
        save_release(ReleaseRecord {
            id: uuid::Uuid::new_v4().to_string(),
            release_name: release_name.clone(),
            group_id: group.id.clone(),
            group_name: group.name.clone(),
            project_ids: projects.iter().map(|project| project.id.clone()).collect(),
            server_ids: servers.iter().map(|server| server.id.clone()).collect(),
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            status: "staged".into(),
        });
        logger.progress(100.0, "完成");
        logger.success(format!(
            "发布 {} 已上传到 {} 台服务器的中转目录，可稍后在「发布历史」中执行替换",
            release_name,
            servers.len()
        ));
        return Ok(());
    }

    replace_phase(
        &config,
        &group,
        &projects,
        &release_name,
        &servers,
        Some(first_conn),
        request.mode,
        request.backup_sibling,
        &logger,
        &cancel,
    )
    .await
}

/// 替换阶段：逐台滚动部署（每台替换并健康检查通过后才动下一台，线上始终保留在服务的机器）
#[allow(clippy::too_many_arguments)]
async fn replace_phase(
    config: &AppConfig,
    group: &BackendGroup,
    projects: &[BackendProject],
    release_name: &str,
    servers: &[ServerConfig],
    first_conn: Option<SshConnection>,
    mode: DeployMode,
    backup_sibling: bool,
    logger: &TaskLogger,
    cancel: &CancellationToken,
) -> Result<()> {
    let date_suffix = chrono::Local::now().format("%Y%m%d").to_string();
    let mut deployed_servers: Vec<String> = Vec::new();
    let total = servers.len().max(1);
    let mut first_conn = first_conn;

    for (index, server) in servers.iter().enumerate() {
        check_cancel(cancel)?;
        logger.progress(
            60.0 + 38.0 * (index as f64 / total as f64),
            format!("部署服务器 {} ({}/{})", server.name, index + 1, servers.len()),
        );
        // 首台复用已打开的连接，其余按需建立
        let conn = match first_conn.take() {
            Some(existing) if index == 0 => existing,
            _ => ssh::connect(config, &server.id).await?,
        };
        for project in projects {
            check_cancel(cancel)?;
            deploy_to_server(
                &conn,
                group,
                project,
                release_name,
                backup_sibling,
                &date_suffix,
                logger,
            )
            .await?;
        }
        deployed_servers.push(server.id.clone());
        logger.success(format!("服务器 {} 全部项目部署完成", server.name));
    }

    // 记录发布历史：从中转替换时优先把原 staged 记录标记为成功
    if mode != DeployMode::Replace || !mark_release_success(&group.id, release_name) {
        save_release(ReleaseRecord {
            id: uuid::Uuid::new_v4().to_string(),
            release_name: release_name.to_string(),
            group_id: group.id.clone(),
            group_name: group.name.clone(),
            project_ids: projects.iter().map(|project| project.id.clone()).collect(),
            server_ids: deployed_servers,
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            status: "success".into(),
        });
    }

    logger.progress(100.0, "完成");
    logger.success(format!("发布 {} 全部完成", release_name));
    Ok(())
}

/// 回滚：从备份目录恢复 bin 并健康检查
pub async fn run_rollback(
    config: AppConfig,
    release_id: String,
    logger: TaskLogger,
    cancel: CancellationToken,
) -> Result<()> {
    let record = load_releases()
        .into_iter()
        .find(|record| record.id == release_id)
        .with_context(|| "找不到该发布记录")?;
    let group = config
        .backend_groups
        .iter()
        .find(|group| group.id == record.group_id)
        .with_context(|| format!("找不到负载组: {}（配置可能已修改）", record.group_id))?
        .clone();
    let projects: Vec<BackendProject> = group
        .projects
        .iter()
        .filter(|project| record.project_ids.contains(&project.id))
        .cloned()
        .collect();
    if projects.is_empty() {
        bail!("发布记录中的项目在当前配置中不存在");
    }

    logger.state("running", format!("回滚 {}", record.release_name));
    logger.warn(format!(
        "开始回滚发布 {}（恢复替换前备份）",
        record.release_name
    ));

    for (server_index, server_id) in record.server_ids.iter().enumerate() {
        check_cancel(&cancel)?;
        let server = config.find_server(server_id)?.clone();
        logger.progress(
            10.0 + 80.0 * (server_index as f64 / record.server_ids.len() as f64),
            format!("回滚服务器 {}", server.name),
        );
        let conn = ssh::connect(&config, &server.id).await?;
        for project in &projects {
            check_cancel(&cancel)?;
            let live_dir = project_live_dir(project);
            let backup_dir = project_backup_dir(&group, &record.release_name, project);
            let script = wrap_project_scripts(
                project,
                &format!(
                    r#"if (-not (Test-Path -LiteralPath '{backup_dir}')) {{ Write-Output '备份目录不存在: {backup_dir}'; $actionExit = 4 }}
else {{
  robocopy '{backup_dir}' '{live_dir}' /E /R:5 /W:2 /NP /NFL /NDL | Out-Null
  if ($LASTEXITCODE -ge 8) {{ Write-Output '恢复失败'; $actionExit = 3 }}
  else {{ Write-Output '已恢复备份' }}
}}
"#,
                    backup_dir = backup_dir.replace('\'', "''"),
                    live_dir = live_dir.replace('\'', "''"),
                ),
            );
            logger.info(format!("[{}] 回滚 {}", server.name, project.name));
            run_ps(&conn, &script, &logger).await?;
            health_check(&conn, project, &logger).await?;
        }
        logger.success(format!("服务器 {} 回滚完成", server.name));
    }

    logger.progress(100.0, "完成");
    record_rollback(&record);
    logger.success(format!("回滚 {} 完成", record.release_name));
    Ok(())
}
