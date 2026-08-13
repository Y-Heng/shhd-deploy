use crate::config::{
    AppConfig, AuthConfig, BackendGroup, BackendProject, CopyMode, ServerConfig,
};
use crate::events::TaskLogger;
use crate::ssh::{self, SshConnection};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

/// 从远程应用目录推导相对站点路径：D:\code\sites\to\service\rest -> to\service\rest
fn relative_site_path(remote_app_dir: &str, fallback_id: &str) -> String {
    let lower = remote_app_dir.to_lowercase();
    if let Some(position) = lower.find("\\sites\\") {
        remote_app_dir[position + "\\sites\\".len()..].to_string()
    } else {
        fallback_id.to_string()
    }
}

/// Windows 路径拼接
fn win_join(base: &str, sub: &str) -> String {
    format!("{}\\{}", base.trim_end_matches('\\'), sub.trim_start_matches('\\'))
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        bail!("任务已被取消");
    }
    Ok(())
}

/// 把本地 bin 目录压缩为 zip（阻塞操作，放到独立线程执行）
fn zip_directory(source_dir: &Path, zip_path: &Path) -> Result<(u64, u64)> {
    use std::io::{Read, Write};
    use zip::write::SimpleFileOptions;

    let zip_file = std::fs::File::create(zip_path)
        .with_context(|| format!("创建压缩包失败: {}", zip_path.display()))?;
    let mut zip_writer = zip::ZipWriter::new(zip_file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);

    let mut file_count: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut read_buffer = vec![0u8; 1024 * 1024];

    for entry in walkdir::WalkDir::new(source_dir) {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(source_dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if relative.is_empty() {
            continue;
        }
        if path.is_dir() {
            zip_writer.add_directory(relative, options)?;
            continue;
        }
        zip_writer.start_file(relative, options)?;
        let mut source_file = std::fs::File::open(path)?;
        loop {
            let read = source_file.read(&mut read_buffer)?;
            if read == 0 {
                break;
            }
            zip_writer.write_all(&read_buffer[..read])?;
            total_bytes += read as u64;
        }
        file_count += 1;
    }
    zip_writer.finish()?;
    Ok((file_count, total_bytes))
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
    let sftp = ssh::open_sftp(conn).await?;
    let remote_sftp_path = ssh::to_sftp_path(remote_path);

    // 确保远端父目录存在
    if let Some(slash_pos) = remote_sftp_path.rfind('/') {
        ssh::sftp_mkdir_all(&sftp, &remote_sftp_path[..slash_pos]).await?;
    }

    let mut local_file = tokio::fs::File::open(local_path)
        .await
        .with_context(|| format!("打开本地文件失败: {}", local_path.display()))?;
    let total_bytes = local_file.metadata().await?.len().max(1);

    let mut remote_file = sftp
        .create(remote_sftp_path.clone())
        .await
        .with_context(|| format!("创建远端文件失败: {}", remote_sftp_path))?;

    let mut buffer = vec![0u8; 256 * 1024];
    let mut sent_bytes: u64 = 0;
    let started = Instant::now();
    let mut last_report = Instant::now();

    loop {
        let read = local_file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        remote_file.write_all(&buffer[..read]).await?;
        sent_bytes += read as u64;
        if last_report.elapsed().as_millis() > 500 {
            let fraction = sent_bytes as f64 / total_bytes as f64;
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
    remote_file.shutdown().await?;

    let elapsed = started.elapsed().as_secs_f64();
    logger.info(format!(
        "上传完成: {:.2} MB，耗时 {:.1} 秒（{:.2} MB/s）",
        total_bytes as f64 / 1024.0 / 1024.0,
        elapsed,
        total_bytes as f64 / 1024.0 / 1024.0 / elapsed.max(0.001)
    ));
    Ok(())
}

/// 在 Windows 服务器上执行 PowerShell 脚本并要求成功
async fn run_ps(conn: &SshConnection, script: &str, logger: &TaskLogger) -> Result<String> {
    let command = ssh::powershell_command(script);
    let mut line_callback = |line: &str| {
        if !line.trim().is_empty() {
            logger.info(format!("  [{}] {}", conn.server.name, line));
        }
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

/// 同目录日期备份：把应用目录复制为 <目录名>-yyyyMMdd（当天已存在则跳过）
async fn sibling_backup(
    conn: &SshConnection,
    target_dir: &str,
    date_suffix: &str,
    logger: &TaskLogger,
) -> Result<()> {
    let backup_dir = format!("{}-{}", target_dir.trim_end_matches('\\'), date_suffix);
    let script = format!(
        r#"$ErrorActionPreference = 'Continue'
if (-not (Test-Path '{target_dir}')) {{ Write-Output '目录不存在，跳过备份'; exit 0 }}
if (Test-Path '{backup_dir}') {{ Write-Output '今日备份已存在({backup_dir})，跳过'; exit 0 }}
robocopy '{target_dir}' '{backup_dir}' /E /R:2 /W:3 /NP /NFL /NDL | Out-Null
if ($LASTEXITCODE -ge 8) {{ Write-Output '备份失败'; exit 2 }}
Write-Output ('目录已备份 -> {backup_dir}')
exit 0"#,
        target_dir = target_dir,
        backup_dir = backup_dir,
    );
    run_ps(conn, &script, logger).await?;
    Ok(())
}

/// 在单台服务器上完成"备份 -> 替换 -> 健康检查"
async fn deploy_to_server(
    conn: &SshConnection,
    group: &BackendGroup,
    project: &BackendProject,
    release_name: &str,
    logger: &TaskLogger,
) -> Result<()> {
    let rel_path = relative_site_path(&project.remote_app_dir, &project.id);
    let app_bin = win_join(&project.remote_app_dir, "bin");
    let staging_bin = win_join(
        &win_join(&win_join(&group.staging_dir, release_name), &rel_path),
        "bin",
    );
    let backup_bin = win_join(
        &win_join(&win_join(&group.backup_dir, release_name), &rel_path),
        "bin",
    );

    logger.info(format!(
        "[{}] 部署 {}: 备份并替换 bin",
        conn.server.name, project.name
    ));

    let script = format!(
        r#"$ErrorActionPreference = 'Continue'
if (-not (Test-Path '{staging_bin}')) {{ Write-Output '暂存目录不存在'; exit 4 }}
if (Test-Path '{app_bin}') {{
  robocopy '{app_bin}' '{backup_bin}' /E /R:2 /W:3 /NP /NFL /NDL | Out-Null
  if ($LASTEXITCODE -ge 8) {{ Write-Output '备份失败'; exit 2 }}
  Write-Output ('备份完成 -> {backup_bin}')
}} else {{
  Write-Output '目标 bin 不存在，跳过备份（首次部署）'
}}
robocopy '{staging_bin}' '{app_bin}' /MIR /R:5 /W:2 /NP /NFL /NDL | Out-Null
if ($LASTEXITCODE -ge 8) {{ Write-Output '替换失败'; exit 3 }}
Write-Output '替换完成'
exit 0"#,
        staging_bin = staging_bin,
        app_bin = app_bin,
        backup_bin = backup_bin,
    );

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
        .chars()
        .next()
        .ok_or_else(|| anyhow!("暂存目录配置为空"))?;
    let path_after_drive = staging_release
        .get(2..)
        .unwrap_or("")
        .trim_start_matches('\\');
    let share_root = format!("\\\\{}\\{}$", secondary.host, drive_letter);
    let remote_share_path = format!("{}\\{}", share_root, path_after_drive);
    let escaped_password = password.replace('\'', "''");
    let escaped_user = secondary.username.replace('\'', "''");

    logger.info(format!(
        "[{}] 通过内网 SMB 复制发布目录到 {} ...",
        source_conn.server.name, secondary.name
    ));

    let script = format!(
        r#"$ErrorActionPreference = 'Continue'
net use '{share_root}' '{password}' /user:'{user}' 2>&1 | Out-Null
robocopy '{staging_release}' '{remote_share_path}' /E /R:2 /W:5 /NP /NFL /NDL | Out-Null
$copyResult = $LASTEXITCODE
net use '{share_root}' /delete /y 2>&1 | Out-Null
if ($copyResult -ge 8) {{ Write-Output ('SMB 复制失败，robocopy 退出码 ' + $copyResult); exit 1 }}
Write-Output '内网复制完成'
exit 0"#,
        share_root = share_root,
        password = escaped_password,
        user = escaped_user,
        staging_release = staging_release,
        remote_share_path = remote_share_path,
    );

    run_ps(source_conn, &script, logger).await?;
    Ok(())
}

/// 把单个项目的产物上传到某台服务器并解压到中转目录
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
    let remote_zip = win_join(
        &win_join(staging_release, "_upload"),
        &format!("{}.zip", project.id),
    );
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
    let rel_path = relative_site_path(&project.remote_app_dir, &project.id);
    let staging_bin = win_join(&win_join(staging_release, &rel_path), "bin");
    let expand_script = format!(
        r#"$ErrorActionPreference = 'Stop'
if (Test-Path '{staging_bin}') {{ Remove-Item -LiteralPath '{staging_bin}' -Recurse -Force }}
Expand-Archive -LiteralPath '{remote_zip}' -DestinationPath '{staging_bin}' -Force
Write-Output '解压完成 -> {staging_bin}'"#,
        staging_bin = staging_bin,
        remote_zip = remote_zip,
    );
    run_ps(conn, &expand_script, logger).await?;
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
        .join("jy-deploy")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&temp_dir)?;
    let mut zip_paths: Vec<(BackendProject, PathBuf)> = Vec::new();
    for project in &projects {
        let zip_path = temp_dir.join(format!("{}.zip", project.id));
        let source_dir = PathBuf::from(&project.local_bin_dir);
        let zip_path_clone = zip_path.clone();
        let (file_count, total_bytes) =
            tokio::task::spawn_blocking(move || zip_directory(&source_dir, &zip_path_clone))
                .await??;
        let zip_size = std::fs::metadata(&zip_path)?.len();
        logger.info(format!(
            "压缩 {}: {} 个文件 {:.2} MB -> {:.2} MB",
            project.name,
            file_count,
            total_bytes as f64 / 1024.0 / 1024.0,
            zip_size as f64 / 1024.0 / 1024.0
        ));
        zip_paths.push((project.clone(), zip_path));
    }
    check_cancel(&cancel)?;

    // 第 3 步：连接首台服务器并上传中转
    logger.progress(10.0, format!("连接服务器 {}", first_server.name));
    let first_conn = ssh::connect(&config, &first_server.id).await?;
    logger.success(format!("已连接 {}", first_server.name));

    let staging_release = win_join(&group.staging_dir, &release_name);
    let cleanup_script = format!(
        r#"$uploadDir = '{}'
if (Test-Path $uploadDir) {{ Remove-Item -LiteralPath $uploadDir -Recurse -Force }}"#,
        win_join(&staging_release, "_upload")
    );
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

    // 第 4 步：同步到组内其他服务器
    let other_servers = &servers[1..];
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
        if backup_sibling {
            for project in projects {
                check_cancel(cancel)?;
                sibling_backup(&conn, &project.remote_app_dir, &date_suffix, logger).await?;
            }
        }
        for project in projects {
            check_cancel(cancel)?;
            deploy_to_server(&conn, group, project, release_name, logger).await?;
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
        "开始回滚发布 {}（恢复替换前备份的 bin）",
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
            let rel_path = relative_site_path(&project.remote_app_dir, &project.id);
            let app_bin = win_join(&project.remote_app_dir, "bin");
            let backup_bin = win_join(
                &win_join(&win_join(&group.backup_dir, &record.release_name), &rel_path),
                "bin",
            );
            let script = format!(
                r#"$ErrorActionPreference = 'Continue'
if (-not (Test-Path '{backup_bin}')) {{ Write-Output '备份目录不存在: {backup_bin}'; exit 4 }}
robocopy '{backup_bin}' '{app_bin}' /MIR /R:5 /W:2 /NP /NFL /NDL | Out-Null
if ($LASTEXITCODE -ge 8) {{ Write-Output '恢复失败'; exit 3 }}
Write-Output '已恢复备份'
exit 0"#,
                backup_bin = backup_bin,
                app_bin = app_bin,
            );
            logger.info(format!("[{}] 回滚 {}", server.name, project.name));
            run_ps(&conn, &script, &logger).await?;
            health_check(&conn, project, &logger).await?;
        }
        logger.success(format!("服务器 {} 回滚完成", server.name));
    }

    logger.progress(100.0, "完成");
    logger.success(format!("回滚 {} 完成", record.release_name));
    Ok(())
}
