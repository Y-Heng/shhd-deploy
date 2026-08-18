use crate::config::AppConfig;
use crate::ssh;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex as StdMutex, OnceLock};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

/// 远端目录条目
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u32,
    /// 以 `.` 开头的隐藏项
    pub hidden: bool,
}

/// 本地待上传文件（相对路径用于保持目录结构）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFileEntry {
    pub local_path: String,
    pub relative_path: String,
}

/// 上传进度事件
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpProgressPayload {
    pub transfer_id: String,
    pub file_name: String,
    pub transferred: u64,
    pub total: u64,
    pub done: bool,
    /// 当前批次中的文件序号（从 1 开始）
    pub file_index: u32,
    /// 当前批次文件总数
    pub file_count: u32,
}

/// 按 server_id 缓存 SSH + SFTP 会话，跳板链 _parents 随连接一并保活
struct CachedSftp {
    conn: ssh::SshConnection,
    sftp: russh_sftp::client::SftpSession,
}

static SFTP_CACHE: OnceLock<Mutex<HashMap<String, CachedSftp>>> = OnceLock::new();
static CANCELLED_TRANSFERS: OnceLock<StdMutex<HashSet<String>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, CachedSftp>> {
    SFTP_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cancelled_transfers() -> &'static StdMutex<HashSet<String>> {
    CANCELLED_TRANSFERS.get_or_init(|| StdMutex::new(HashSet::new()))
}

fn lock_cancelled() -> std::sync::MutexGuard<'static, HashSet<String>> {
    cancelled_transfers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 标记该次上传为用户终止，正在写入的文件会在下一分块退出
pub fn request_cancel(transfer_id: &str) {
    lock_cancelled().insert(transfer_id.to_string());
}

fn is_cancelled(transfer_id: &str) -> bool {
    lock_cancelled().contains(transfer_id)
}

fn clear_cancel(transfer_id: &str) {
    lock_cancelled().remove(transfer_id);
}

async fn invalidate_cache(server_id: &str) {
    cache().lock().await.remove(server_id);
}

/// 获取或建立缓存会话；连接已关闭时自动重建
async fn ensure_cached(config: &AppConfig, server_id: &str) -> Result<()> {
    let mut sessions = cache().lock().await;
    let needs_connect = match sessions.get(server_id) {
        Some(entry) => entry.conn.is_closed(),
        None => true,
    };
    if needs_connect {
        sessions.remove(server_id);
        let conn = ssh::connect(config, server_id).await?;
        let sftp = ssh::open_sftp(&conn).await?;
        sessions.insert(server_id.to_string(), CachedSftp { conn, sftp });
    }
    Ok(())
}

fn join_remote(parent: &str, name: &str) -> String {
    let base = ssh::to_sftp_path(parent);
    if base.is_empty() || base == "/" {
        format!("/{}", name.trim_start_matches('/'))
    } else if base.ends_with('/') {
        format!("{}{}", base, name)
    } else {
        format!("{}/{}", base, name)
    }
}

fn emit_progress(
    app: &AppHandle,
    transfer_id: &str,
    file_name: &str,
    transferred: u64,
    total: u64,
    done: bool,
    file_index: u32,
    file_count: u32,
) {
    let _ = app.emit(
        "sftp-progress",
        SftpProgressPayload {
            transfer_id: transfer_id.to_string(),
            file_name: file_name.to_string(),
            transferred,
            total,
            done,
            file_index,
            file_count,
        },
    );
}

/// Windows 注册表事务/页面文件等无法可靠拷贝，上传时直接跳过
fn should_skip_upload_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".regtrans-ms") || lower.ends_with(".blf") { return true; }
    if lower.starts_with("ntuser.dat") || lower.starts_with("usrclass.dat") { return true; }
    matches!(
        lower.as_str(),
        "pagefile.sys"
            | "hiberfil.sys"
            | "swapfile.sys"
            | "dumpstack.log.tmp"
            | "thumbs.db"
            | "desktop.ini"
            | "$recycle.bin"
            | "system volume information"
    )
}

/// 隐藏+系统属性的文件通常被系统占用，SFTP 创建会返回 Failure
fn should_skip_upload_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if should_skip_upload_name(name) { return true; }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
        if let Ok(metadata) = path.metadata() {
            let attributes = metadata.file_attributes();
            if attributes & FILE_ATTRIBUTE_HIDDEN != 0 && attributes & FILE_ATTRIBUTE_SYSTEM != 0 {
                return true;
            }
        }
    }
    false
}

/// 收集待上传文件：传入文件则返回单条，传入目录则递归保留相对路径
pub fn collect_local_files(root: &str) -> Result<Vec<LocalFileEntry>> {
    let root_path = Path::new(root);
    if root_path.is_file() {
        if should_skip_upload_path(root_path) { return Ok(vec![]); }
        let file_name = root_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("upload.bin")
            .to_string();
        return Ok(vec![LocalFileEntry {
            local_path: root_path.to_string_lossy().to_string(),
            relative_path: file_name,
        }]);
    }
    if !root_path.is_dir() {
        bail!("路径不存在或不可读: {}", root);
    }
    // 拖入文件夹时保留文件夹本身，避免内容被摊到目标目录根下
    let folder_name = root_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string());
    let mut files = Vec::new();
    let walker = walkdir::WalkDir::new(root_path).into_iter().filter_entry(|entry| {
        let name = entry.file_name().to_string_lossy();
        !should_skip_upload_name(&name)
    });
    for entry in walker.flatten() {
        if !entry.file_type().is_file() { continue; }
        let absolute = entry.path();
        if should_skip_upload_path(absolute) { continue; }
        // 被占用的文件打开即失败，避免整批上传被 SFTP Failure 打断
        if std::fs::File::open(absolute).is_err() { continue; }
        let relative = absolute
            .strip_prefix(root_path)
            .with_context(|| format!("计算相对路径失败: {}", absolute.display()))?;
        let stripped = relative.to_string_lossy().replace('\\', "/");
        if stripped.is_empty() { continue; }
        let relative_path = match &folder_name {
            Some(name) => format!("{}/{}", name, stripped),
            None => stripped,
        };
        files.push(LocalFileEntry {
            local_path: absolute.to_string_lossy().to_string(),
            relative_path,
        });
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

/// 列出远端目录
pub async fn list_dir(config: &AppConfig, server_id: &str, path: &str) -> Result<Vec<SftpEntry>> {
    let remote_path = {
        let normalized = ssh::to_sftp_path(path);
        if normalized.is_empty() {
            "/".into()
        } else {
            normalized
        }
    };

    for attempt in 0..2 {
        ensure_cached(config, server_id).await?;
        let sessions = cache().lock().await;
        let sftp = &sessions
            .get(server_id)
            .context("SFTP 会话缓存异常")?
            .sftp;

        let list_result = async {
            let entries = sftp
                .read_dir(remote_path.clone())
                .await
                .with_context(|| format!("读取远端目录失败: {}", remote_path))?;

            let mut result = Vec::new();
            for entry in entries {
                let name = entry.file_name();
                if name == "." || name == ".." {
                    continue;
                }
                let attributes = entry.metadata();
                let is_dir = attributes.is_dir();
                result.push(SftpEntry {
                    name: name.clone(),
                    path: join_remote(&remote_path, &name),
                    is_dir,
                    size: attributes.size.unwrap_or(0),
                    mtime: attributes.mtime.unwrap_or(0),
                    hidden: name.starts_with('.'),
                });
            }

            result.sort_by(|left, right| match (left.is_dir, right.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
            });
            Ok(result)
        }
        .await;

        drop(sessions);
        match list_result {
            Ok(value) => return Ok(value),
            Err(error) => {
                invalidate_cache(server_id).await;
                if attempt == 0 {
                    continue;
                }
                return Err(error);
            }
        }
    }
    bail!("读取远端目录失败")
}

/// 上传本地文件到远端路径（远端为完整文件路径），并通过事件推送进度
pub async fn upload_file(
    app: &AppHandle,
    config: &AppConfig,
    server_id: &str,
    local_path: &str,
    remote_path: &str,
    transfer_id: &str,
    file_index: u32,
    file_count: u32,
) -> Result<()> {
    let remote = ssh::to_sftp_path(remote_path);
    let parent_dir = remote.rfind('/').map(|slash_pos| remote[..slash_pos].to_string());

    let file_name = PathBuf::from(local_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("upload.bin")
        .to_string();

    let mut local_file = tokio::fs::File::open(local_path)
        .await
        .with_context(|| format!("打开本地文件失败: {}", local_path))?;
    let total = local_file
        .metadata()
        .await
        .with_context(|| format!("读取本地文件大小失败: {}", local_path))?
        .len();

    if is_cancelled(transfer_id) {
        clear_cancel(transfer_id);
        bail!("上传已取消");
    }

    emit_progress(
        app,
        transfer_id,
        &file_name,
        0,
        total,
        false,
        file_index,
        file_count,
    );

    // 流式分块写入，边读边传，进度按真实字节更新
    for attempt in 0..2 {
        ensure_cached(config, server_id).await?;
        let sessions = cache().lock().await;
        let cached = sessions.get(server_id).context("SFTP 会话缓存异常")?;
        let chunk_size = ssh::sftp_write_chunk(cached.conn.server.os);
        let sftp = &cached.sftp;

        let upload_result = async {
            if let Some(parent) = &parent_dir {
                if !parent.is_empty() {
                    ssh::sftp_mkdir_all(sftp, parent).await?;
                }
            }

            let _ = sftp.remove_file(remote.clone()).await;
            let mut remote_handle = sftp
                .create(remote.clone())
                .await
                .with_context(|| format!("创建远端文件失败: {}", remote))?;

            if total == 0 {
                remote_handle.shutdown().await?;
                emit_progress(app, transfer_id, &file_name, 0, 0, true, file_index, file_count);
                return Ok(());
            }

            let mut buffer = vec![0u8; chunk_size];
            let mut transferred = 0u64;
            loop {
                if is_cancelled(transfer_id) {
                    let _ = remote_handle.shutdown().await;
                    let _ = sftp.remove_file(remote.clone()).await;
                    bail!("上传已取消");
                }
                let read_len = local_file.read(&mut buffer).await?;
                if read_len == 0 {
                    break;
                }
                remote_handle.write_all(&buffer[..read_len]).await?;
                transferred += read_len as u64;
                emit_progress(
                    app,
                    transfer_id,
                    &file_name,
                    transferred,
                    total,
                    false,
                    file_index,
                    file_count,
                );
            }
            remote_handle.flush().await?;
            remote_handle.shutdown().await?;

            if let Ok(meta) = sftp.metadata(remote.clone()).await {
                if let Some(remote_size) = meta.size {
                    if remote_size != total {
                        bail!(
                            "远端文件大小不符: {} 期望 {} 实际 {}",
                            remote,
                            total,
                            remote_size
                        );
                    }
                }
            }
            emit_progress(
                app,
                transfer_id,
                &file_name,
                total,
                total,
                true,
                file_index,
                file_count,
            );
            Ok(())
        }
        .await;

        drop(sessions);
        match upload_result {
            Ok(()) => {
                clear_cancel(transfer_id);
                return Ok(());
            }
            Err(error) => {
                if is_cancelled(transfer_id) {
                    clear_cancel(transfer_id);
                    return Err(error);
                }
                invalidate_cache(server_id).await;
                if attempt == 0 {
                    local_file
                        .seek(std::io::SeekFrom::Start(0))
                        .await
                        .with_context(|| format!("重试前重置本地文件指针失败: {}", local_path))?;
                    continue;
                }
                return Err(error);
            }
        }
    }
    bail!("上传失败")
}

/// 下载远端文件到本地路径
pub async fn download_file(
    config: &AppConfig,
    server_id: &str,
    remote_path: &str,
    local_path: &str,
) -> Result<()> {
    let remote = ssh::to_sftp_path(remote_path);
    let mut content = Vec::new();

    for attempt in 0..2 {
        ensure_cached(config, server_id).await?;
        let sessions = cache().lock().await;
        let sftp = &sessions
            .get(server_id)
            .context("SFTP 会话缓存异常")?
            .sftp;

        let download_result = async {
            let mut remote_handle = sftp
                .open(remote.clone())
                .await
                .with_context(|| format!("打开远端文件失败: {}", remote))?;
            let mut buffer = Vec::new();
            remote_handle
                .read_to_end(&mut buffer)
                .await
                .with_context(|| format!("读取远端文件失败: {}", remote))?;
            Ok(buffer)
        }
        .await;

        drop(sessions);
        match download_result {
            Ok(buffer) => {
                content = buffer;
                break;
            }
            Err(error) => {
                invalidate_cache(server_id).await;
                if attempt == 0 {
                    continue;
                }
                return Err(error);
            }
        }
    }

    if let Some(parent) = PathBuf::from(local_path).parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("创建本地目录失败: {}", parent.display()))?;
    }
    tokio::fs::write(local_path, content)
        .await
        .with_context(|| format!("写入本地文件失败: {}", local_path))?;
    Ok(())
}

/// 新建远端目录
pub async fn mkdir(config: &AppConfig, server_id: &str, path: &str) -> Result<()> {
    let path_owned = path.to_string();
    for attempt in 0..2 {
        ensure_cached(config, server_id).await?;
        let sessions = cache().lock().await;
        let sftp = &sessions
            .get(server_id)
            .context("SFTP 会话缓存异常")?
            .sftp;
        let result = ssh::sftp_mkdir_all(sftp, &path_owned).await;
        drop(sessions);
        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                invalidate_cache(server_id).await;
                if attempt == 0 {
                    continue;
                }
                return Err(error);
            }
        }
    }
    bail!("创建远端目录失败")
}

/// 删除远端文件或目录（目录递归）
pub async fn remove_path(config: &AppConfig, server_id: &str, path: &str) -> Result<()> {
    let path_owned = ssh::to_sftp_path(path);
    for attempt in 0..2 {
        ensure_cached(config, server_id).await?;
        let sessions = cache().lock().await;
        let sftp = &sessions
            .get(server_id)
            .context("SFTP 会话缓存异常")?
            .sftp;
        let result = remove_recursive(sftp, &path_owned).await;
        drop(sessions);
        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                invalidate_cache(server_id).await;
                if attempt == 0 {
                    continue;
                }
                return Err(error);
            }
        }
    }
    bail!("删除远端路径失败")
}

async fn remove_recursive(
    sftp: &russh_sftp::client::SftpSession,
    path: &str,
) -> Result<()> {
    let metadata = sftp
        .metadata(path.to_string())
        .await
        .with_context(|| format!("获取远端路径信息失败: {}", path))?;
    if metadata.is_dir() {
        let entries = sftp
            .read_dir(path.to_string())
            .await
            .with_context(|| format!("读取待删目录失败: {}", path))?;
        for entry in entries {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let child = join_remote(path, &name);
            Box::pin(remove_recursive(sftp, &child)).await?;
        }
        sftp.remove_dir(path.to_string())
            .await
            .with_context(|| format!("删除远端目录失败: {}", path))?;
    } else {
        sftp.remove_file(path.to_string())
            .await
            .with_context(|| format!("删除远端文件失败: {}", path))?;
    }
    Ok(())
}

/// 重命名 / 移动
pub async fn rename(
    config: &AppConfig,
    server_id: &str,
    from_path: &str,
    to_path: &str,
) -> Result<()> {
    let from = ssh::to_sftp_path(from_path);
    let to = ssh::to_sftp_path(to_path);
    if from == to {
        bail!("源路径与目标路径相同");
    }
    let parent_dir = to.rfind('/').map(|slash_pos| to[..slash_pos].to_string());

    for attempt in 0..2 {
        ensure_cached(config, server_id).await?;
        let sessions = cache().lock().await;
        let sftp = &sessions
            .get(server_id)
            .context("SFTP 会话缓存异常")?
            .sftp;

        let rename_result = async {
            if let Some(parent) = &parent_dir {
                if !parent.is_empty() && parent != "/" {
                    ssh::sftp_mkdir_all(sftp, parent).await?;
                }
            }
            sftp.rename(from.clone(), to.clone())
                .await
                .with_context(|| format!("重命名失败: {} -> {}", from, to))
        }
        .await;

        drop(sessions);
        match rename_result {
            Ok(()) => return Ok(()),
            Err(error) => {
                invalidate_cache(server_id).await;
                if attempt == 0 {
                    continue;
                }
                return Err(error);
            }
        }
    }
    bail!("重命名失败")
}

/// 断开指定服务器的 SFTP 缓存
pub async fn disconnect(server_id: &str) {
    invalidate_cache(server_id).await;
}
