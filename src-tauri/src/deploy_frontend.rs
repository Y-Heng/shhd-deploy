use crate::config::{AppConfig, OsType};
use crate::deploy_backend::DeployMode;
use crate::events::TaskLogger;
use crate::ssh::{self, SshConnection};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// 前端部署选项
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendDeployOptions {
    /// full=直接替换；stage=仅上传到中转；replace=从中转替换
    #[serde(default = "default_mode")]
    pub mode: DeployMode,
    /// 替换前备份线上目录，供发布历史回滚
    #[serde(default)]
    pub backup_sibling: bool,
}

fn default_mode() -> DeployMode {
    DeployMode::Full
}

/// 前端发布历史
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendReleaseRecord {
    pub id: String,
    pub created_at: String,
    pub mode: String,
    pub group_name: String,
    pub target_ids: Vec<String>,
    pub target_names: Vec<String>,
    pub server_names: Vec<String>,
    #[serde(default)]
    pub server_ids: Vec<String>,
    /// 线上快照后缀：{remoteDir}.rollback-{suffix}，无则不可回滚
    #[serde(default)]
    pub backup_suffix: Option<String>,
    pub status: String,
    pub message: String,
}

pub fn load_frontend_releases() -> Vec<FrontendReleaseRecord> {
    let path = crate::config::frontend_releases_file_path();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn persist_frontend_releases(records: &[FrontendReleaseRecord]) {
    let _ = std::fs::create_dir_all(crate::config::config_dir());
    if let Ok(content) = serde_json::to_string_pretty(records) {
        let _ = std::fs::write(crate::config::frontend_releases_file_path(), content);
    }
}

fn save_frontend_release(record: FrontendReleaseRecord) {
    let mut records = load_frontend_releases();
    records.insert(0, record);
    records.truncate(100);
    persist_frontend_releases(&records);
}

/// 回滚成功后：原发布标为已回滚，并追加一条回滚记录
fn record_frontend_rollback(source: &FrontendReleaseRecord) {
    let mut records = load_frontend_releases();
    for record in records.iter_mut() {
        if record.id == source.id && (record.status == "success" || record.status == "failed") {
            record.status = "rolled_back".into();
        }
    }
    records.insert(
        0,
        FrontendReleaseRecord {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            mode: "回滚".into(),
            group_name: source.group_name.clone(),
            target_ids: source.target_ids.clone(),
            target_names: source.target_names.clone(),
            server_names: source.server_names.clone(),
            server_ids: source.server_ids.clone(),
            backup_suffix: None,
            status: "rollback".into(),
            message: format!("已回滚到 {} 发布前的备份", source.created_at),
        },
    );
    records.truncate(100);
    persist_frontend_releases(&records);
}

fn mode_label(mode: DeployMode) -> &'static str {
    match mode {
        DeployMode::Full => "直接替换",
        DeployMode::Stage => "仅上传到中转",
        DeployMode::Replace => "从中转替换",
    }
}

fn trim_dir(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

fn escape_ps(value: &str) -> String {
    value.replace('\'', "''")
}

fn escape_sh(value: &str) -> String {
    value.replace('\'', "'\\''")
}

fn native_path(os: OsType, path: &str) -> String {
    match os {
        OsType::Windows => path.replace('/', "\\"),
        OsType::Linux => path.replace('\\', "/"),
    }
}

fn parent_dir(path: &str) -> String {
    let normalized = trim_dir(path).replace('\\', "/");
    match normalized.rfind('/') {
        Some(pos) => normalized[..pos].to_string(),
        None => normalized,
    }
}

fn join_native(os: OsType, parent: &str, name: &str) -> String {
    match os {
        OsType::Windows => format!("{}\\{}", native_path(os, parent).trim_end_matches('\\'), name),
        OsType::Linux => format!("{}/{}", native_path(os, parent).trim_end_matches('/'), name),
    }
}

fn rollback_backup_dir(live_dir: &str, suffix: &str) -> String {
    format!("{}.rollback-{}", trim_dir(live_dir), suffix)
}

/// 按服务器系统执行脚本：Windows 用 PowerShell，Linux 用 sh
async fn run_os_script(conn: &SshConnection, script: &str, logger: &TaskLogger) -> Result<()> {
    let command = match conn.server.os {
        OsType::Windows => ssh::powershell_command(script),
        OsType::Linux => ssh::shell_command(script),
    };
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

/// 一段进度区间
struct ProgressSpan {
    start: f64,
    end: f64,
}

impl ProgressSpan {
    fn at(&self, fraction: f64) -> f64 {
        self.start + (self.end - self.start) * fraction.clamp(0.0, 1.0)
    }

    fn slice(&self, from: f64, to: f64) -> ProgressSpan {
        ProgressSpan {
            start: self.at(from),
            end: self.at(to),
        }
    }

    fn width(&self) -> f64 {
        self.end - self.start
    }
}

/// 长时间操作期间持续刷新进度条，并每隔几秒写一条日志，避免界面一直停在 0%
async fn with_heartbeat<T, Fut>(
    logger: &TaskLogger,
    cancel: &CancellationToken,
    span: &ProgressSpan,
    step: &str,
    fut: Fut,
) -> Result<T>
where
    Fut: Future<Output = Result<T>>,
{
    logger.info(format!("▶ {}", step));
    logger.progress(span.start, step.to_string());
    let started = Instant::now();
    tokio::pin!(fut);
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await;
    let mut last_log = Instant::now();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => bail!("任务已被取消"),
            result = &mut fut => return result,
            _ = ticker.tick() => {
                let elapsed = started.elapsed().as_secs();
                let ratio = (elapsed as f64 / 120.0).min(0.9);
                logger.progress(span.at(ratio), format!("{}（已 {} 秒）", step, elapsed));
                if last_log.elapsed().as_secs() >= 4 {
                    logger.info(format!("… {}仍在进行，已 {} 秒，请稍候", step, elapsed));
                    last_log = Instant::now();
                }
            }
        }
    }
}

/// 目标的中转目录：优先用自定义配置，留空时默认 <remote_dir>-staging
fn staging_dir_of(target: &crate::config::FrontendTarget) -> String {
    match &target.staging_dir {
        Some(dir) if !dir.trim().is_empty() => trim_dir(dir.trim()).to_string(),
        _ => format!("{}-staging", trim_dir(&target.remote_dir)),
    }
}

/// 把本地目录打成 zip（前端产物已压缩，用 Stored 加快打包）
fn zip_directory(source_dir: &Path, zip_path: &Path) -> Result<(u64, u64)> {
    use std::io::{Read, Write};
    use zip::write::SimpleFileOptions;

    if !source_dir.is_dir() {
        bail!("本地目录不存在: {}", source_dir.display());
    }
    let zip_file = std::fs::File::create(zip_path)
        .with_context(|| format!("创建压缩包失败: {}", zip_path.display()))?;
    let mut zip_writer = zip::ZipWriter::new(zip_file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
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

/// SFTP 上传单个文件，带进度与一次重试
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
    let chunk_size = ssh::sftp_write_chunk(conn.server.os);
    logger.info(format!(
        "▶ {}（{:.2} MB）",
        step_name,
        total_bytes as f64 / 1024.0 / 1024.0
    ));
    logger.progress(progress_base, format!("{} 0%", step_name));

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
        if let Some(slash_pos) = remote_sftp_path.rfind('/') {
            ssh::sftp_mkdir_all(&sftp, &remote_sftp_path[..slash_pos]).await?;
        }

        match upload_file_once(
            &sftp,
            &mut local_file,
            &remote_sftp_path,
            total_bytes,
            logger,
            progress_base,
            progress_span,
            step_name,
            chunk_size,
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
    if let Some(error) = last_error {
        return Err(error);
    }

    let elapsed = started.elapsed().as_secs_f64();
    logger.info(format!(
        "上传完成: {:.2} MB，耗时 {:.1} 秒（{:.2} MB/s）",
        total_bytes as f64 / 1024.0 / 1024.0,
        elapsed,
        total_bytes as f64 / 1024.0 / 1024.0 / elapsed.max(0.001)
    ));
    Ok(())
}

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
    let mut last_progress = Instant::now();
    let mut last_log = Instant::now();
    let progress_total = total_bytes.max(1);

    loop {
        let read = local_file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        remote_file.write_all(&buffer[..read]).await.with_context(|| {
            format!(
                "写入远端失败（已上传 {:.2} MB / {:.2} MB）: {}",
                sent_bytes as f64 / 1024.0 / 1024.0,
                total_bytes as f64 / 1024.0 / 1024.0,
                remote_sftp_path
            )
        })?;
        sent_bytes += read as u64;
        let should_progress = last_progress.elapsed().as_millis() > 300;
        let should_log = last_log.elapsed().as_millis() > 1500;
        if should_progress || should_log {
            let fraction = sent_bytes as f64 / progress_total as f64;
            let sent_mb = sent_bytes as f64 / 1024.0 / 1024.0;
            let total_mb = total_bytes as f64 / 1024.0 / 1024.0;
            let speed_mbps = sent_mb / started.elapsed().as_secs_f64().max(0.001);
            let step_text = format!(
                "{} {:.1}%（{:.2}/{:.2} MB，{:.2} MB/s）",
                step_name,
                fraction * 100.0,
                sent_mb,
                total_mb,
                speed_mbps
            );
            if should_progress {
                logger.progress(progress_base + progress_span * fraction, step_text.clone());
                last_progress = Instant::now();
            }
            if should_log {
                logger.info(format!("上传中 {}", step_text));
                last_log = Instant::now();
            }
        }
    }
    remote_file.shutdown().await?;
    if sent_bytes != total_bytes {
        bail!(
            "上传字节数不完整: 已传 {} / {}",
            sent_bytes,
            total_bytes
        );
    }
    Ok(())
}

/// 远端解压 zip 到目标目录；delete_extraneous 时先解到临时目录再整目录替换
async fn extract_zip(
    conn: &SshConnection,
    zip_path: &str,
    dest_dir: &str,
    delete_extraneous: bool,
    logger: &TaskLogger,
) -> Result<()> {
    let zip_path = native_path(conn.server.os, zip_path);
    let dest_dir = native_path(conn.server.os, dest_dir);
    let script = match conn.server.os {
        OsType::Windows => {
            let copy_switch = if delete_extraneous { "/MIR" } else { "/E" };
            format!(
                r#"$ErrorActionPreference = 'Continue'
$zip = '{zip}'
$dest = '{dest}'
$extract = if ({delete}) {{ '{dest}.extract-tmp' }} else {{ $dest }}
if (-not (Test-Path -LiteralPath $zip)) {{ Write-Output '压缩包不存在'; exit 4 }}
if (Test-Path -LiteralPath $extract) {{ if ($extract -ne $dest) {{ Remove-Item -LiteralPath $extract -Recurse -Force }} }}
if (-not (Test-Path -LiteralPath $extract)) {{ New-Item -ItemType Directory -Path $extract -Force | Out-Null }}
Write-Output '开始解压压缩包（文件较多时需要一些时间）…'
$tar = Get-Command tar -ErrorAction SilentlyContinue
if ($tar) {{
  & tar -xf $zip -C $extract
  if ($LASTEXITCODE -ne 0) {{ Write-Output 'tar 解压失败'; exit 6 }}
}} else {{
  Expand-Archive -LiteralPath $zip -DestinationPath $extract -Force
}}
Write-Output '压缩包已解压'
if ({delete}) {{
  Write-Output '正在同步到线上目录…'
  if (-not (Test-Path -LiteralPath $dest)) {{ New-Item -ItemType Directory -Path $dest -Force | Out-Null }}
  robocopy $extract $dest {copy_switch} /R:2 /W:2 /NP /NFL /NDL | Out-Null
  if ($LASTEXITCODE -ge 8) {{ Write-Output '同步到目标目录失败'; exit 3 }}
  Remove-Item -LiteralPath $extract -Recurse -Force -ErrorAction SilentlyContinue
  Write-Output '线上目录已更新'
}}
Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
Write-Output '解压覆盖完成'
exit 0"#,
                zip = escape_ps(&zip_path),
                dest = escape_ps(&dest_dir),
                delete = if delete_extraneous { "$true" } else { "$false" },
                copy_switch = copy_switch,
            )
        }
        OsType::Linux => {
            let dest = escape_sh(&dest_dir);
            let zip = escape_sh(&zip_path);
            if delete_extraneous {
                format!(
                    r#"zip='{zip}'
dest='{dest}'
extract="${{dest}}.extract-tmp"
if [ ! -f "$zip" ]; then echo '压缩包不存在'; exit 4; fi
echo '开始解压压缩包（文件较多时需要一些时间）…'
rm -rf "$extract"
mkdir -p "$extract"
if command -v unzip >/dev/null 2>&1; then
  unzip -o -q "$zip" -d "$extract" || exit 6
elif command -v python3 >/dev/null 2>&1; then
  python3 -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$zip" "$extract" || exit 6
elif command -v python >/dev/null 2>&1; then
  python -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$zip" "$extract" || exit 6
else
  echo '无法解压：请安装 unzip 或 python3'
  exit 6
fi
echo '压缩包已解压，正在写入线上目录…'
if command -v rsync >/dev/null 2>&1; then
  mkdir -p "$dest"
  rsync -a --delete "$extract"/ "$dest"/ || exit 3
  rm -rf "$extract"
else
  rm -rf "$dest"
  mv "$extract" "$dest" || exit 3
fi
rm -f "$zip"
echo '解压覆盖完成'"#,
                    zip = zip,
                    dest = dest,
                )
            } else {
                format!(
                    r#"zip='{zip}'
dest='{dest}'
if [ ! -f "$zip" ]; then echo '压缩包不存在'; exit 4; fi
echo '开始解压压缩包到线上目录（文件较多时需要一些时间）…'
mkdir -p "$dest"
if command -v unzip >/dev/null 2>&1; then
  unzip -o -q "$zip" -d "$dest" || exit 6
elif command -v python3 >/dev/null 2>&1; then
  python3 -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$zip" "$dest" || exit 6
elif command -v python >/dev/null 2>&1; then
  python -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$zip" "$dest" || exit 6
else
  echo '无法解压：请安装 unzip 或 python3'
  exit 6
fi
rm -f "$zip"
echo '解压覆盖完成'"#,
                    zip = zip,
                    dest = dest,
                )
            }
        }
    };
    run_os_script(conn, &script, logger).await
}

/// 发布前把线上目录复制为独立快照（每次发布一份，供回滚）
async fn snapshot_backup(
    conn: &SshConnection,
    live_dir: &str,
    backup_dir: &str,
    logger: &TaskLogger,
) -> Result<()> {
    let live = native_path(conn.server.os, trim_dir(live_dir));
    let backup = native_path(conn.server.os, trim_dir(backup_dir));
    let script = match conn.server.os {
        OsType::Windows => format!(
            r#"$ErrorActionPreference = 'Continue'
if (-not (Test-Path -LiteralPath '{live}')) {{ Write-Output '线上目录不存在，跳过备份'; exit 0 }}
Write-Output '开始备份线上目录（文件较多时需要几分钟）…'
if (Test-Path -LiteralPath '{backup}') {{ Remove-Item -LiteralPath '{backup}' -Recurse -Force }}
robocopy '{live}' '{backup}' /E /R:2 /W:3 /NP /NFL /NDL | Out-Null
if ($LASTEXITCODE -ge 8) {{ Write-Output '备份失败'; exit 2 }}
Write-Output ('备份完成 -> {backup}')
exit 0"#,
            live = escape_ps(&live),
            backup = escape_ps(&backup),
        ),
        OsType::Linux => format!(
            r#"if [ ! -d '{live}' ]; then echo '线上目录不存在，跳过备份'; exit 0; fi
echo '开始备份线上目录（文件较多时需要几分钟）…'
rm -rf '{backup}'
cp -a '{live}' '{backup}' && echo '备份完成 -> {backup}'"#,
            live = escape_sh(&live),
            backup = escape_sh(&backup),
        ),
    };
    run_os_script(conn, &script, logger).await
}

/// 把备份目录恢复到线上
async fn restore_from_backup(
    conn: &SshConnection,
    backup_dir: &str,
    live_dir: &str,
    logger: &TaskLogger,
) -> Result<()> {
    let live = native_path(conn.server.os, trim_dir(live_dir));
    let backup = native_path(conn.server.os, trim_dir(backup_dir));
    let script = match conn.server.os {
        OsType::Windows => format!(
            r#"$ErrorActionPreference = 'Continue'
if (-not (Test-Path -LiteralPath '{backup}')) {{ Write-Output '备份目录不存在: {backup}'; exit 4 }}
Write-Output '开始从备份恢复线上目录…'
if (-not (Test-Path -LiteralPath '{live}')) {{ New-Item -ItemType Directory -Path '{live}' -Force | Out-Null }}
robocopy '{backup}' '{live}' /MIR /R:5 /W:2 /NP /NFL /NDL | Out-Null
if ($LASTEXITCODE -ge 8) {{ Write-Output '恢复失败'; exit 3 }}
Write-Output '已从备份恢复到 {live}'
exit 0"#,
            backup = escape_ps(&backup),
            live = escape_ps(&live),
        ),
        OsType::Linux => format!(
            r#"if [ ! -d '{backup}' ]; then echo '备份目录不存在: {backup}'; exit 4; fi
echo '开始从备份恢复线上目录…'
mkdir -p '{live}'
if command -v rsync >/dev/null 2>&1; then
  rsync -a --delete '{backup}/' '{live}/' && echo '已从备份恢复到 {live}'
else
  rm -rf '{live}'
  cp -a '{backup}' '{live}' && echo '已从备份恢复到 {live}'
fi"#,
            backup = escape_sh(&backup),
            live = escape_sh(&live),
        ),
    };
    run_os_script(conn, &script, logger).await
}

/// 服务器端把中转目录内容替换到正式目录
async fn replace_from_staging(
    conn: &SshConnection,
    staging: &str,
    live: &str,
    delete_extraneous: bool,
    logger: &TaskLogger,
) -> Result<()> {
    let staging = native_path(conn.server.os, trim_dir(staging));
    let live = native_path(conn.server.os, trim_dir(live));
    let script = match conn.server.os {
        OsType::Windows => {
            let copy_switch = if delete_extraneous { "/MIR" } else { "/E" };
            let extra_hint = if delete_extraneous { "（含删除多余文件）" } else { "" };
            format!(
                r#"$ErrorActionPreference = 'Continue'
if (-not (Test-Path -LiteralPath '{staging}')) {{ Write-Output '中转目录不存在: {staging}'; exit 4 }}
Write-Output '开始把中转目录同步到线上…'
if (-not (Test-Path -LiteralPath '{live}')) {{ New-Item -ItemType Directory -Path '{live}' -Force | Out-Null }}
robocopy '{staging}' '{live}' {copy_switch} /R:5 /W:2 /NP /NFL /NDL | Out-Null
if ($LASTEXITCODE -ge 8) {{ Write-Output '替换失败'; exit 3 }}
Write-Output '已同步中转内容到 {live}{extra_hint}'
exit 0"#,
                staging = escape_ps(&staging),
                live = escape_ps(&live),
                copy_switch = copy_switch,
                extra_hint = extra_hint,
            )
        }
        OsType::Linux => {
            if delete_extraneous {
                format!(
                    r#"if [ ! -d '{staging}' ]; then echo '中转目录不存在: {staging}'; exit 4; fi
echo '开始把中转目录同步到线上…'
if ! command -v rsync >/dev/null 2>&1; then echo '服务器未安装 rsync，无法执行“删除多余文件”的替换，请安装 rsync 或关闭该选项'; exit 5; fi
mkdir -p '{live}'
rsync -a --delete '{staging}/' '{live}/' && echo '已同步中转内容到 {live}（含删除多余文件）'"#,
                    staging = escape_sh(&staging),
                    live = escape_sh(&live),
                )
            } else {
                format!(
                    r#"if [ ! -d '{staging}' ]; then echo '中转目录不存在: {staging}'; exit 4; fi
echo '开始把中转目录复制到线上…'
mkdir -p '{live}'
cp -af '{staging}/.' '{live}/' && echo '已复制中转内容到 {live}'"#,
                    staging = escape_sh(&staging),
                    live = escape_sh(&live),
                )
            }
        }
    };
    run_os_script(conn, &script, logger).await
}

struct TempZip(PathBuf);

impl Drop for TempZip {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
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

    let mode_text = mode_label(options.mode);
    let target_id_list: Vec<String> = targets.iter().map(|target| target.id.clone()).collect();
    let target_names: Vec<String> = targets.iter().map(|target| target.name.clone()).collect();
    let mut group_names: Vec<String> = targets
        .iter()
        .map(|target| target.group.clone().unwrap_or_else(|| "未分组".into()))
        .collect();
    group_names.sort();
    group_names.dedup();
    let group_name = group_names.join("、");
    let mut server_names: Vec<String> = Vec::new();
    let mut server_ids: Vec<String> = Vec::new();
    for target in &targets {
        for server_id in &target.server_ids {
            if server_ids.iter().any(|id| id == server_id) {
                continue;
            }
            server_ids.push(server_id.clone());
            if let Ok(server) = config.find_server(server_id) {
                server_names.push(server.name.clone());
            }
        }
    }

    let backup_suffix = if options.mode != DeployMode::Stage && options.backup_sibling {
        let stamp_id = uuid::Uuid::new_v4().to_string().replace('-', "");
        Some(format!(
            "{}-{}",
            chrono::Local::now().format("%Y%m%d%H%M%S"),
            &stamp_id[..8]
        ))
    } else {
        None
    };

    logger.progress(1.0, "开始部署");
    logger.info(format!(
        "开始前端部署 | 方式：{} | 环境：{} | 项目：{} | 服务器：{}",
        mode_text,
        group_name,
        target_names.join("、"),
        server_names.join("、")
    ));
    match options.mode {
        DeployMode::Stage => logger.info("步骤：打包本地产物 → 连接服务器 → 上传压缩包 → 解压到中转目录"),
        DeployMode::Full => logger.info("步骤：打包本地产物 → 连接服务器 → 备份线上目录 → 上传压缩包 → 服务器解压覆盖"),
        DeployMode::Replace => logger.info("步骤：连接服务器 → 备份线上目录 → 从中转目录替换到线上"),
    };

    let deploy_result = deploy_frontend_targets(
        &config,
        &targets,
        &options,
        backup_suffix.as_deref(),
        &logger,
        &cancel,
        mode_text,
    )
    .await;

    save_frontend_release(FrontendReleaseRecord {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        mode: mode_text.to_string(),
        group_name,
        target_ids: target_id_list,
        target_names,
        server_names,
        server_ids,
        backup_suffix,
        status: match (&deploy_result, options.mode) {
            (Ok(()), DeployMode::Stage) => "staged".into(),
            (Ok(()), _) => "success".into(),
            (Err(_), _) => "failed".into(),
        },
        message: match &deploy_result {
            Ok(()) => format!("前端部署完成（{}）", mode_text),
            Err(error) => error.to_string().chars().take(500).collect(),
        },
    });
    deploy_result
}

async fn deploy_frontend_targets(
    config: &AppConfig,
    targets: &[crate::config::FrontendTarget],
    options: &FrontendDeployOptions,
    backup_suffix: Option<&str>,
    logger: &TaskLogger,
    cancel: &CancellationToken,
    mode_text: &str,
) -> Result<()> {
    let server_count: usize = targets
        .iter()
        .map(|target| target.server_ids.len())
        .sum::<usize>()
        .max(1);
    let pack_count = if options.mode == DeployMode::Replace {
        0
    } else {
        targets.len()
    };
    let pack_share = if pack_count > 0 { 8.0 } else { 0.0 };
    let pack_each = if pack_count > 0 {
        pack_share / pack_count as f64
    } else {
        0.0
    };
    let per_server = (100.0 - pack_share) / server_count as f64;
    let mut packed_done = 0usize;
    let mut finished_servers = 0usize;

    for target in targets {
        if cancel.is_cancelled() {
            bail!("任务已被取消");
        }
        logger.info(format!("======== 项目：{} ========", target.name));

        let packed_zip = if options.mode == DeployMode::Replace {
            None
        } else {
            let pack_span = ProgressSpan {
                start: packed_done as f64 * pack_each,
                end: (packed_done as f64 + 1.0) * pack_each,
            };
            let archive_id = uuid::Uuid::new_v4().to_string();
            let local_zip = std::env::temp_dir().join(format!("shhd-fe-{}.zip", archive_id));
            let source_dir = PathBuf::from(&target.local_dir);
            let zip_path = local_zip.clone();
            let (file_count, total_bytes) = with_heartbeat(
                logger,
                cancel,
                &pack_span,
                &format!("[{}] 打包本地产物 {}", target.name, target.local_dir),
                async {
                    let packed = tokio::task::spawn_blocking(move || zip_directory(&source_dir, &zip_path))
                        .await
                        .context("打包任务异常")??;
                    Ok(packed)
                },
            )
            .await?;
            let zip_size = tokio::fs::metadata(&local_zip).await?.len();
            logger.info(format!(
                "打包完成：{} 个文件，原始 {:.2} MB，压缩包 {:.2} MB",
                file_count,
                total_bytes as f64 / 1024.0 / 1024.0,
                zip_size as f64 / 1024.0 / 1024.0
            ));
            packed_done += 1;
            Some((TempZip(local_zip), archive_id))
        };

        let staging_dir = staging_dir_of(target);

        for server_id in &target.server_ids {
            if cancel.is_cancelled() {
                bail!("任务已被取消");
            }
            let server = config.find_server(server_id)?.clone();
            let os_label = match server.os {
                OsType::Windows => "Windows",
                OsType::Linux => "Linux",
            };
            let server_span = ProgressSpan {
                start: pack_share + finished_servers as f64 * per_server,
                end: pack_share + (finished_servers as f64 + 1.0) * per_server,
            };
            let label = format!("{} → {}", target.name, server.name);
            let conn = with_heartbeat(
                logger,
                cancel,
                &server_span.slice(0.0, 0.08),
                &format!("[{}] 连接服务器（{}）", label, os_label),
                async { ssh::connect(config, server_id).await },
            )
            .await?;
            logger.info(format!("已连接 {}", server.name));

            match options.mode {
                DeployMode::Stage => {
                    let (temp_zip, archive_id) = packed_zip.as_ref().expect("stage 需要打包");
                    let remote_zip = join_native(
                        conn.server.os,
                        &parent_dir(&staging_dir),
                        &format!(".shhd-fe-{}.zip", archive_id),
                    );
                    let upload_span = server_span.slice(0.08, 0.72);
                    upload_file(
                        &conn,
                        &temp_zip.0,
                        &remote_zip,
                        logger,
                        upload_span.start,
                        upload_span.width(),
                        &format!("[{}] 上传压缩包到中转", label),
                    )
                    .await?;
                    with_heartbeat(
                        logger,
                        cancel,
                        &server_span.slice(0.72, 1.0),
                        &format!("[{}] 解压到中转目录 {}", label, staging_dir),
                        extract_zip(&conn, &remote_zip, &staging_dir, true, logger),
                    )
                    .await?;
                    logger.success(format!("{} 中转完成", label));
                }
                DeployMode::Full => {
                    if let Some(suffix) = backup_suffix {
                        with_heartbeat(
                            logger,
                            cancel,
                            &server_span.slice(0.08, 0.32),
                            &format!("[{}] 备份线上目录（用于回滚）", label),
                            snapshot_backup(
                                &conn,
                                &target.remote_dir,
                                &rollback_backup_dir(&target.remote_dir, suffix),
                                logger,
                            ),
                        )
                        .await?;
                    }
                    let (temp_zip, archive_id) = packed_zip.as_ref().expect("full 需要打包");
                    let remote_zip = join_native(
                        conn.server.os,
                        &parent_dir(&target.remote_dir),
                        &format!(".shhd-fe-{}.zip", archive_id),
                    );
                    let upload_from = if backup_suffix.is_some() { 0.32 } else { 0.08 };
                    let upload_span = server_span.slice(upload_from, 0.72);
                    upload_file(
                        &conn,
                        &temp_zip.0,
                        &remote_zip,
                        logger,
                        upload_span.start,
                        upload_span.width(),
                        &format!("[{}] 上传压缩包", label),
                    )
                    .await?;
                    with_heartbeat(
                        logger,
                        cancel,
                        &server_span.slice(0.72, 1.0),
                        &format!("[{}] 服务器解压并覆盖线上目录", label),
                        extract_zip(
                            &conn,
                            &remote_zip,
                            &target.remote_dir,
                            target.delete_extraneous,
                            logger,
                        ),
                    )
                    .await?;
                    logger.success(format!("{} 部署完成", label));
                }
                DeployMode::Replace => {
                    if let Some(suffix) = backup_suffix {
                        with_heartbeat(
                            logger,
                            cancel,
                            &server_span.slice(0.08, 0.40),
                            &format!("[{}] 备份线上目录（用于回滚）", label),
                            snapshot_backup(
                                &conn,
                                &target.remote_dir,
                                &rollback_backup_dir(&target.remote_dir, suffix),
                                logger,
                            ),
                        )
                        .await?;
                    }
                    let replace_from = if backup_suffix.is_some() { 0.40 } else { 0.08 };
                    with_heartbeat(
                        logger,
                        cancel,
                        &server_span.slice(replace_from, 1.0),
                        &format!("[{}] 从中转目录替换到线上", label),
                        replace_from_staging(
                            &conn,
                            &staging_dir,
                            &target.remote_dir,
                            target.delete_extraneous,
                            logger,
                        ),
                    )
                    .await?;
                    logger.success(format!("{} 已从中转替换", label));
                }
            }
            finished_servers += 1;
            logger.progress(
                pack_share + finished_servers as f64 * per_server,
                format!("{} 本机完成", label),
            );
        }
    }

    logger.progress(100.0, "全部完成");
    logger.success(format!("前端部署完成（{}）", mode_text));
    Ok(())
}

/// 回滚：把线上目录恢复为该次发布前的快照
pub async fn run_frontend_rollback(
    config: AppConfig,
    release_id: String,
    logger: TaskLogger,
    cancel: CancellationToken,
) -> Result<()> {
    let record = load_frontend_releases()
        .into_iter()
        .find(|item| item.id == release_id)
        .context("找不到该前端发布记录")?;
    if record.status != "success" && record.status != "failed" {
        bail!("只有带备份的成功或失败发布才能回滚（当前状态: {}）", record.status);
    }
    let suffix = record
        .backup_suffix
        .as_deref()
        .filter(|value| !value.is_empty())
        .context("该记录没有发布前备份，无法回滚（请勾选「替换前备份」后再发布）")?;

    let targets: Vec<_> = config
        .frontend_targets
        .iter()
        .filter(|target| record.target_ids.contains(&target.id))
        .cloned()
        .collect();
    if targets.is_empty() {
        bail!("发布记录中的项目在当前配置中不存在，无法回滚");
    }

    logger.progress(1.0, "开始回滚");
    logger.warn(format!(
        "开始回滚 {}（{}）到发布前备份",
        record.target_names.join("、"),
        record.group_name
    ));
    logger.info("步骤：连接服务器 → 用发布前快照覆盖线上目录");

    let total_steps = targets
        .iter()
        .map(|target| target.server_ids.len())
        .sum::<usize>()
        .max(1);
    let mut finished_steps = 0usize;

    for target in &targets {
        for server_id in &target.server_ids {
            if cancel.is_cancelled() {
                bail!("任务已被取消");
            }
            let server = config.find_server(server_id)?.clone();
            let os_label = match server.os {
                OsType::Windows => "Windows",
                OsType::Linux => "Linux",
            };
            let span = ProgressSpan {
                start: (finished_steps as f64 / total_steps as f64) * 100.0,
                end: ((finished_steps as f64 + 1.0) / total_steps as f64) * 100.0,
            };
            let label = format!("{} → {}", target.name, server.name);
            let conn = with_heartbeat(
                &logger,
                &cancel,
                &span.slice(0.0, 0.12),
                &format!("[{}] 连接服务器（{}）", label, os_label),
                async { ssh::connect(&config, server_id).await },
            )
            .await?;
            with_heartbeat(
                &logger,
                &cancel,
                &span.slice(0.12, 1.0),
                &format!("[{}] 从备份恢复线上目录", label),
                restore_from_backup(
                    &conn,
                    &rollback_backup_dir(&target.remote_dir, suffix),
                    &target.remote_dir,
                    &logger,
                ),
            )
            .await?;
            logger.success(format!("{} 已恢复", label));
            finished_steps += 1;
        }
    }

    logger.progress(100.0, "完成");
    record_frontend_rollback(&record);
    logger.success("前端回滚完成");
    Ok(())
}
