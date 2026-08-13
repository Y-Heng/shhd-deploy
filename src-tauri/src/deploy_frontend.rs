use crate::config::AppConfig;
use crate::deploy_backend::DeployMode;
use crate::events::TaskLogger;
use crate::ssh::{self, SshConnection};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

/// 前端部署选项
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendDeployOptions {
    /// full=直接替换；stage=仅上传到中转；replace=从中转替换
    #[serde(default = "default_mode")]
    pub mode: DeployMode,
    /// 替换前把目标目录复制为 <目录名>-yyyyMMdd（当天已存在则跳过）
    #[serde(default)]
    pub backup_sibling: bool,
}

fn default_mode() -> DeployMode {
    DeployMode::Full
}

/// 本地文件信息
struct LocalFileInfo {
    absolute_path: PathBuf,
    size: u64,
    mtime_secs: u32,
}

/// 收集本地目录下所有文件（键为使用正斜杠的相对路径）
fn collect_local_files(local_dir: &str) -> Result<HashMap<String, LocalFileInfo>> {
    let base = PathBuf::from(local_dir);
    if !base.is_dir() {
        bail!("本地目录不存在: {}", local_dir);
    }
    let mut files = HashMap::new();
    for entry in walkdir::WalkDir::new(&base) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&base)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = entry.metadata()?;
        let mtime_secs = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as u32)
            .unwrap_or(0);
        files.insert(
            relative,
            LocalFileInfo {
                absolute_path: entry.path().to_path_buf(),
                size: metadata.len(),
                mtime_secs,
            },
        );
    }
    Ok(files)
}

/// 远端文件信息（大小 + 修改时间）
struct RemoteFileInfo {
    size: u64,
    mtime_secs: u32,
}

/// 递归列出远端目录所有文件
async fn collect_remote_files(
    sftp: &russh_sftp::client::SftpSession,
    base_dir: &str,
) -> Result<HashMap<String, RemoteFileInfo>> {
    let mut files = HashMap::new();
    let mut pending_dirs = vec![String::new()];
    while let Some(relative_dir) = pending_dirs.pop() {
        let full_dir = if relative_dir.is_empty() {
            base_dir.to_string()
        } else {
            format!("{}/{}", base_dir, relative_dir)
        };
        let entries = match sftp.read_dir(full_dir).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let relative_path = if relative_dir.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", relative_dir, name)
            };
            let attributes = entry.metadata();
            if attributes.is_dir() {
                pending_dirs.push(relative_path);
            } else {
                files.insert(
                    relative_path,
                    RemoteFileInfo {
                        size: attributes.size.unwrap_or(0),
                        mtime_secs: attributes.mtime.unwrap_or(0),
                    },
                );
            }
        }
    }
    Ok(files)
}

/// 把本地文件增量同步到远端目录，返回（上传数, 跳过数, 删除数）
async fn sync_directory(
    sftp: &russh_sftp::client::SftpSession,
    local_files: &HashMap<String, LocalFileInfo>,
    remote_base: &str,
    delete_extraneous: bool,
    cancel: &CancellationToken,
) -> Result<(u64, u64, u64)> {
    ssh::sftp_mkdir_all(sftp, remote_base).await?;
    let remote_files = collect_remote_files(sftp, remote_base).await?;

    let mut uploaded_count = 0u64;
    let mut skipped_count = 0u64;
    for (relative_path, local_file) in local_files {
        if cancel.is_cancelled() {
            bail!("任务已被取消");
        }
        // 大小一致且远端时间不早于本地 -> 未变化，跳过
        if let Some(remote_file) = remote_files.get(relative_path) {
            if remote_file.size == local_file.size
                && remote_file.mtime_secs + 1 >= local_file.mtime_secs
            {
                skipped_count += 1;
                continue;
            }
        }
        let remote_path = format!("{}/{}", remote_base, relative_path);
        if let Some(slash_position) = remote_path.rfind('/') {
            ssh::sftp_mkdir_all(sftp, &remote_path[..slash_position]).await?;
        }
        let content = tokio::fs::read(&local_file.absolute_path)
            .await
            .with_context(|| {
                format!("读取本地文件失败: {}", local_file.absolute_path.display())
            })?;
        let mut remote_handle = sftp.create(remote_path.clone()).await?;
        remote_handle.write_all(&content).await?;
        remote_handle.shutdown().await?;

        // 同步修改时间，保证下次增量比较有效
        let mut attributes = russh_sftp::protocol::FileAttributes::default();
        attributes.atime = Some(local_file.mtime_secs);
        attributes.mtime = Some(local_file.mtime_secs);
        let _ = sftp.set_metadata(remote_path, attributes).await;
        uploaded_count += 1;
    }

    // 删除远端多余文件（可选）
    let mut deleted_count = 0u64;
    if delete_extraneous {
        for relative_path in remote_files.keys() {
            if !local_files.contains_key(relative_path) {
                let remote_path = format!("{}/{}", remote_base, relative_path);
                if sftp.remove_file(remote_path).await.is_ok() {
                    deleted_count += 1;
                }
            }
        }
    }
    Ok((uploaded_count, skipped_count, deleted_count))
}

/// 在 Linux 服务器上执行脚本并要求成功
async fn run_sh(conn: &SshConnection, script: &str, logger: &TaskLogger) -> Result<()> {
    let command = ssh::shell_command(script);
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
    Ok(())
}

/// 目标的中转目录：优先用自定义配置，留空时默认 <remote_dir>-staging
fn staging_dir_of(target: &crate::config::FrontendTarget) -> String {
    match &target.staging_dir {
        Some(dir) if !dir.trim().is_empty() => dir.trim().trim_end_matches('/').to_string(),
        _ => format!("{}-staging", target.remote_dir.trim_end_matches('/')),
    }
}

/// 同目录日期备份（Linux）：目录 -> 目录-yyyyMMdd（当天已存在则跳过）
async fn sibling_backup_linux(
    conn: &SshConnection,
    target_dir: &str,
    date_suffix: &str,
    logger: &TaskLogger,
) -> Result<()> {
    let live = target_dir.trim_end_matches('/');
    let backup = format!("{}-{}", live, date_suffix);
    let script = format!(
        r#"if [ ! -d '{live}' ]; then echo '目录不存在，跳过备份'; exit 0; fi
if [ -d '{backup}' ]; then echo '今日备份已存在({backup})，跳过'; exit 0; fi
cp -a '{live}' '{backup}' && echo '目录已备份 -> {backup}'"#,
        live = live,
        backup = backup,
    );
    run_sh(conn, &script, logger).await
}

/// 服务器端把中转目录内容替换到正式目录
async fn replace_from_staging(
    conn: &SshConnection,
    staging: &str,
    live: &str,
    delete_extraneous: bool,
    logger: &TaskLogger,
) -> Result<()> {
    let staging = staging.trim_end_matches('/');
    let live = live.trim_end_matches('/');
    let script = if delete_extraneous {
        format!(
            r#"if [ ! -d '{staging}' ]; then echo '中转目录不存在: {staging}'; exit 4; fi
if ! command -v rsync >/dev/null 2>&1; then echo '服务器未安装 rsync，无法执行“删除多余文件”的替换，请安装 rsync 或关闭该选项'; exit 5; fi
mkdir -p '{live}'
rsync -a --delete '{staging}/' '{live}/' && echo '已同步中转内容到 {live}（含删除多余文件）'"#,
            staging = staging,
            live = live,
        )
    } else {
        format!(
            r#"if [ ! -d '{staging}' ]; then echo '中转目录不存在: {staging}'; exit 4; fi
mkdir -p '{live}'
cp -af '{staging}/.' '{live}/' && echo '已复制中转内容到 {live}'"#,
            staging = staging,
            live = live,
        )
    };
    run_sh(conn, &script, logger).await
}

/// 前端部署：支持直接替换 / 仅上传中转 / 从中转替换
pub async fn run_frontend_deploy(
    config: AppConfig,
    target_ids: Vec<String>,
    options: FrontendDeployOptions,
    logger: TaskLogger,
    cancel: CancellationToken,
) -> Result<()> {
    let targets: Vec<_> = config
        .frontend_targets
        .iter()
        .filter(|target| target_ids.contains(&target.id))
        .cloned()
        .collect();
    if targets.is_empty() {
        bail!("未选择任何前端部署目标");
    }

    let mode_text = match options.mode {
        DeployMode::Full => "直接替换",
        DeployMode::Stage => "仅上传到中转",
        DeployMode::Replace => "从中转替换",
    };
    logger.info(format!("前端部署模式：{}", mode_text));

    let date_suffix = chrono::Local::now().format("%Y%m%d").to_string();
    let total_steps: usize = targets
        .iter()
        .map(|target| target.server_ids.len())
        .sum::<usize>()
        .max(1);
    let mut finished_steps = 0usize;

    for target in &targets {
        logger.info(format!("=== 部署 {} ===", target.name));

        // 从中转替换不需要本地文件
        let local_files = if options.mode == DeployMode::Replace {
            HashMap::new()
        } else {
            let files = collect_local_files(&target.local_dir)?;
            let total_size: u64 = files.values().map(|file| file.size).sum();
            logger.info(format!(
                "本地产物: {} 个文件，共 {:.2} MB",
                files.len(),
                total_size as f64 / 1024.0 / 1024.0
            ));
            files
        };

        let staging_dir = staging_dir_of(target);

        for server_id in &target.server_ids {
            if cancel.is_cancelled() {
                bail!("任务已被取消");
            }
            let server = config.find_server(server_id)?.clone();
            logger.progress(
                (finished_steps as f64 / total_steps as f64) * 100.0,
                format!("{} -> {}", target.name, server.name),
            );
            logger.info(format!("连接服务器 {} ...", server.name));
            let conn = ssh::connect(&config, server_id).await?;

            match options.mode {
                DeployMode::Stage => {
                    // 中转目录与本地保持完全一致（含删除），替换时才是精确内容
                    let sftp = ssh::open_sftp(&conn).await?;
                    let (uploaded, skipped, deleted) =
                        sync_directory(&sftp, &local_files, &staging_dir, true, &cancel).await?;
                    logger.success(format!(
                        "{} -> {} 中转完成({}): 上传 {} 个，跳过 {} 个，清理 {} 个",
                        target.name, server.name, staging_dir, uploaded, skipped, deleted
                    ));
                }
                DeployMode::Full => {
                    if options.backup_sibling {
                        sibling_backup_linux(&conn, &target.remote_dir, &date_suffix, &logger)
                            .await?;
                    }
                    let sftp = ssh::open_sftp(&conn).await?;
                    let remote_base = ssh::to_sftp_path(&target.remote_dir);
                    let (uploaded, skipped, deleted) = sync_directory(
                        &sftp,
                        &local_files,
                        &remote_base,
                        target.delete_extraneous,
                        &cancel,
                    )
                    .await?;
                    logger.success(format!(
                        "{} -> {}: 上传 {} 个，跳过 {} 个未变化，删除 {} 个",
                        target.name, server.name, uploaded, skipped, deleted
                    ));
                }
                DeployMode::Replace => {
                    if options.backup_sibling {
                        sibling_backup_linux(&conn, &target.remote_dir, &date_suffix, &logger)
                            .await?;
                    }
                    replace_from_staging(
                        &conn,
                        &staging_dir,
                        &target.remote_dir,
                        target.delete_extraneous,
                        &logger,
                    )
                    .await?;
                    logger.success(format!(
                        "{} -> {}: 已从中转目录替换",
                        target.name, server.name
                    ));
                }
            }
            finished_steps += 1;
        }
    }

    logger.progress(100.0, "完成");
    logger.success(format!("前端部署完成（{}）", mode_text));
    Ok(())
}
