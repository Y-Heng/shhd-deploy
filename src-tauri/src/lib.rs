mod config;
mod deploy_backend;
mod deploy_frontend;
mod docker;
mod events;
mod local_fs;
mod logger;
mod mcp;
mod sftp_browser;
mod ssh;
mod terminal;
mod tunnel;

use config::AppConfig;
use deploy_backend::{BackendDeployRequest, ReleaseRecord};
use events::{TaskLogger, TaskRegistry};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use terminal::TerminalManager;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tunnel::{TunnelManager, TunnelStatusInfo};

/// SFTP 错误统一记录诊断日志
fn map_sftp_error(operation: &str, server_id: &str, path: &str, error: impl std::fmt::Display) -> String {
    let message = format!("{:#}", error);
    logger::append_log(&format!(
        "sftp {} 错误 [{}:{}]: {}",
        operation, server_id, path, message
    ));
    message
}

/// 全局应用状态
pub struct AppState {
    pub config: Arc<RwLock<AppConfig>>,
    pub tunnels: Arc<TunnelManager>,
    pub terminals: Arc<TerminalManager>,
    pub tasks: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pub task_registry: Arc<TaskRegistry>,
    pub mcp: Arc<mcp::McpManager>,
}

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Result<AppConfig, String> {
    Ok(state.config.read().await.clone())
}

#[tauri::command]
async fn save_config(
    app: AppHandle,
    state: State<'_, AppState>,
    config: AppConfig,
) -> Result<(), String> {
    config::save(&config).map_err(|error| {
        logger::append_log(&format!("save_config 失败: {}", error));
        error.to_string()
    })?;
    logger::set_enabled(config.logging.enabled);
    logger::append_log("save_config 成功");
    *state.config.write().await = config;
    // MCP 设置可能变化，重新应用
    state.mcp.apply(app).await;
    Ok(())
}

#[tauri::command]
fn get_config_path() -> String {
    config::config_file_path().to_string_lossy().to_string()
}

/// 导出配置到指定路径
#[tauri::command]
async fn export_config(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let config = state.config.read().await.clone();
    let content = serde_json::to_string_pretty(&config).map_err(|error| error.to_string())?;
    std::fs::write(&path, content).map_err(|error| format!("写入 {} 失败: {}", path, error))?;
    Ok(())
}

/// 从指定路径导入配置（校验格式后覆盖当前配置）
#[tauri::command]
async fn import_config(state: State<'_, AppState>, path: String) -> Result<AppConfig, String> {
    let content =
        std::fs::read_to_string(&path).map_err(|error| format!("读取 {} 失败: {}", path, error))?;
    let imported: AppConfig =
        serde_json::from_str(&content).map_err(|error| format!("配置格式错误: {}", error))?;
    config::save(&imported).map_err(|error| error.to_string())?;
    logger::set_enabled(imported.logging.enabled);
    *state.config.write().await = imported.clone();
    Ok(imported)
}

/// MCP 服务当前状态（运行中返回端口）
#[tauri::command]
async fn get_mcp_status(state: State<'_, AppState>) -> Result<Option<u16>, String> {
    Ok(state.mcp.running_port().await)
}

/// 测试服务器连通性，返回系统信息与耗时
#[tauri::command]
async fn test_server(state: State<'_, AppState>, server_id: String) -> Result<String, String> {
    let config = state.config.read().await.clone();
    probe_server(&state, &config, &server_id, true).await
}

/// 用表单草稿测试（未保存也可测，不影响已保存配置）
#[tauri::command]
async fn test_server_draft(
    state: State<'_, AppState>,
    server: config::ServerConfig,
) -> Result<String, String> {
    let saved = state.config.read().await.clone();
    let existed_before = saved.servers.iter().any(|item| item.id == server.id);
    let mut config = saved.clone();
    if let Some(index) = config.servers.iter().position(|item| item.id == server.id) {
        config.servers[index] = server.clone();
    } else {
        config.servers.push(server.clone());
    }
    probe_server(&state, &config, &server.id, existed_before).await
}

/// 将探测到的系统类型写回配置
async fn persist_detected_os(
    state: &State<'_, AppState>,
    server_id: &str,
    detected: &str,
) -> Result<(), String> {
    let mut config = state.config.read().await.clone();
    let server = config
        .servers
        .iter_mut()
        .find(|item| item.id == server_id);
    if server.is_none() { return Ok(()); }
    let server = server.unwrap();
    if server.detected_os.as_deref() == Some(detected) { return Ok(()); }
    server.detected_os = Some(detected.to_string());
    config::save(&config).map_err(|error| error.to_string())?;
    *state.config.write().await = config;
    Ok(())
}

async fn probe_server(
    state: &State<'_, AppState>,
    config: &config::AppConfig,
    server_id: &str,
    persist: bool,
) -> Result<String, String> {
    let started = std::time::Instant::now();
    let conn = ssh::connect(config, server_id)
        .await
        .map_err(|error| {
            let message = format!("{:#}", error);
            logger::append_log(&format!("ssh connect 失败 [{}]: {}", server_id, message));
            message
        })?;
    let connect_millis = started.elapsed().as_millis();

    let (detected, raw_output) = ssh::probe_os(&conn)
        .await
        .map_err(|error| format!("{:#}", error))?;

    if persist {
        if let Some(os) = &detected {
            persist_detected_os(state, server_id, os).await?;
        }
    }

    let os_label = detected.as_deref().unwrap_or("未知");
    Ok(format!(
        "连接成功，耗时 {} ms\n探测系统: {}\n{}",
        connect_millis,
        os_label,
        raw_output
    ))
}

#[tauri::command]
async fn start_tunnel(
    app: AppHandle,
    state: State<'_, AppState>,
    tunnel_id: String,
) -> Result<(), String> {
    let config = state.config.read().await.clone();
    if let Err(error) = state.tunnels.start(app, config, &tunnel_id).await {
        let message = format!("{:#}", error);
        logger::append_log(&format!("tunnel start 失败 [{}]: {}", tunnel_id, message));
        // 配置缺失等异常仅记日志，由隧道状态栏展示，不弹前端 toast
    }
    Ok(())
}

#[tauri::command]
async fn stop_tunnel(state: State<'_, AppState>, tunnel_id: String) -> Result<(), String> {
    state.tunnels.stop(&tunnel_id).await;
    Ok(())
}

#[tauri::command]
async fn tunnel_status(state: State<'_, AppState>) -> Result<Vec<TunnelStatusInfo>, String> {
    let config = state.config.read().await.clone();
    Ok(state.tunnels.status_all(&config).await)
}

/// 注册一个可取消的后台任务并返回任务 ID
async fn register_task(state: &AppState) -> (String, CancellationToken) {
    let task_id = uuid::Uuid::new_v4().to_string();
    let token = CancellationToken::new();
    state
        .tasks
        .lock()
        .await
        .insert(task_id.clone(), token.clone());
    (task_id, token)
}

/// 统一的任务收尾：推送最终状态并清理任务表
async fn finish_task(
    tasks: Arc<Mutex<HashMap<String, CancellationToken>>>,
    logger: TaskLogger,
    cancel: CancellationToken,
    result: anyhow::Result<()>,
) {
    match result {
        Ok(()) => logger.state("success", "任务完成"),
        Err(error) => {
            if cancel.is_cancelled() {
                logger.warn("任务已取消");
                logger.state("cancelled", "任务已取消");
            } else {
                logger.error(format!("{:#}", error));
                logger.state("failed", format!("{:#}", error));
            }
        }
    }
    tasks.lock().await.remove(&logger.task_id);
}

/// 启动后端部署任务（界面与 MCP 共用）
pub(crate) async fn launch_backend_deploy(app: &AppHandle, request: BackendDeployRequest) -> String {
    let state: State<AppState> = app.state();
    let (task_id, cancel) = register_task(&state).await;
    let logger = TaskLogger::new(app.clone(), task_id.clone(), state.task_registry.clone());
    let config = state.config.read().await.clone();
    let tasks = state.tasks.clone();
    tauri::async_runtime::spawn(async move {
        let result =
            deploy_backend::run_backend_deploy(config, request, logger.clone(), cancel.clone())
                .await;
        finish_task(tasks, logger, cancel, result).await;
    });
    task_id
}

/// 启动回滚任务（界面与 MCP 共用）
pub(crate) async fn launch_rollback(app: &AppHandle, release_id: String) -> String {
    let state: State<AppState> = app.state();
    let (task_id, cancel) = register_task(&state).await;
    let logger = TaskLogger::new(app.clone(), task_id.clone(), state.task_registry.clone());
    let config = state.config.read().await.clone();
    let tasks = state.tasks.clone();
    tauri::async_runtime::spawn(async move {
        let result =
            deploy_backend::run_rollback(config, release_id, logger.clone(), cancel.clone()).await;
        finish_task(tasks, logger, cancel, result).await;
    });
    task_id
}

/// 启动前端部署任务（界面与 MCP 共用）
pub(crate) async fn launch_frontend_deploy(
    app: &AppHandle,
    target_ids: Vec<String>,
    options: deploy_frontend::FrontendDeployOptions,
) -> String {
    let state: State<AppState> = app.state();
    let (task_id, cancel) = register_task(&state).await;
    let logger = TaskLogger::new(app.clone(), task_id.clone(), state.task_registry.clone());
    let config = state.config.read().await.clone();
    let tasks = state.tasks.clone();
    tauri::async_runtime::spawn(async move {
        let result = deploy_frontend::run_frontend_deploy(
            config,
            target_ids,
            options,
            logger.clone(),
            cancel.clone(),
        )
        .await;
        finish_task(tasks, logger, cancel, result).await;
    });
    task_id
}

/// 启动 Docker 部署任务（界面与 MCP 共用）
pub(crate) async fn launch_docker_deploy(app: &AppHandle, target_id: String) -> String {
    let state: State<AppState> = app.state();
    let (task_id, cancel) = register_task(&state).await;
    let logger = TaskLogger::new(app.clone(), task_id.clone(), state.task_registry.clone());
    let config = state.config.read().await.clone();
    let tasks = state.tasks.clone();
    tauri::async_runtime::spawn(async move {
        let result =
            docker::run_docker_deploy(config, target_id, logger.clone(), cancel.clone()).await;
        finish_task(tasks, logger, cancel, result).await;
    });
    task_id
}

#[tauri::command]
async fn start_backend_deploy(
    app: AppHandle,
    request: BackendDeployRequest,
) -> Result<String, String> {
    Ok(launch_backend_deploy(&app, request).await)
}

#[tauri::command]
async fn start_rollback(app: AppHandle, release_id: String) -> Result<String, String> {
    Ok(launch_rollback(&app, release_id).await)
}

#[tauri::command]
fn get_releases() -> Vec<ReleaseRecord> {
    deploy_backend::load_releases()
}

#[tauri::command]
async fn start_frontend_deploy(
    app: AppHandle,
    target_ids: Vec<String>,
    options: deploy_frontend::FrontendDeployOptions,
) -> Result<String, String> {
    Ok(launch_frontend_deploy(&app, target_ids, options).await)
}

#[tauri::command]
async fn start_docker_deploy(app: AppHandle, target_id: String) -> Result<String, String> {
    Ok(launch_docker_deploy(&app, target_id).await)
}

#[tauri::command]
async fn cancel_task(state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    if let Some(token) = state.tasks.lock().await.get(&task_id) {
        token.cancel();
    }
    Ok(())
}

/// 打开 SSH 终端会话
#[tauri::command]
async fn terminal_open(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
    cols: u32,
    rows: u32,
) -> Result<String, String> {
    let config = state.config.read().await.clone();
    let result = state
        .terminals
        .open(app.clone(), config, &server_id, cols, rows)
        .await;
    match &result {
        Ok(session_id) => logger::append_log(&format!(
            "terminal open 成功 [{}] session={}",
            server_id, session_id
        )),
        Err(error) => logger::append_log(&format!(
            "terminal open 失败 [{}]: {:#}",
            server_id, error
        )),
    }
    let session_id = result.map_err(|error| format!("{:#}", error))?;

    // 后台探测系统类型并写回配置，不阻塞终端打开
    let server_id_probe = server_id.clone();
    tokio::spawn(async move {
        let state: State<AppState> = app.state();
        let config = state.config.read().await.clone();
        if let Ok(conn) = ssh::connect(&config, &server_id_probe).await {
            if let Ok((detected, _)) = ssh::probe_os(&conn).await {
                if let Some(os) = detected {
                    let _ = persist_detected_os(&state, &server_id_probe, &os).await;
                }
            }
        }
    });

    Ok(session_id)
}

#[tauri::command]
async fn terminal_write(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    state.terminals.write(&session_id, &data).await;
    Ok(())
}

#[tauri::command]
async fn terminal_resize(
    state: State<'_, AppState>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    state.terminals.resize(&session_id, cols, rows).await;
    Ok(())
}

#[tauri::command]
async fn terminal_close(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.terminals.close(&session_id).await;
    Ok(())
}

#[tauri::command]
fn get_home_dir() -> Result<String, String> {
    local_fs::get_home_dir().map_err(|error| format!("{:#}", error))
}

#[tauri::command]
fn list_local_drives() -> Result<Vec<local_fs::LocalDirEntry>, String> {
    local_fs::list_local_drives().map_err(|error| format!("{:#}", error))
}

#[tauri::command]
fn list_local_dir(path: String) -> Result<Vec<local_fs::LocalDirEntry>, String> {
    local_fs::list_local_dir(&path).map_err(|error| format!("{:#}", error))
}

#[tauri::command]
async fn sftp_list(
    state: State<'_, AppState>,
    server_id: String,
    path: String,
) -> Result<Vec<sftp_browser::SftpEntry>, String> {
    let config = state.config.read().await.clone();
    sftp_browser::list_dir(&config, &server_id, &path)
        .await
        .map_err(|error| map_sftp_error("list", &server_id, &path, error))
}

#[tauri::command]
async fn sftp_upload(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
    local_path: String,
    remote_path: String,
    transfer_id: String,
    file_index: Option<u32>,
    file_count: Option<u32>,
) -> Result<(), String> {
    let config = state.config.read().await.clone();
    sftp_browser::upload_file(
        &app,
        &config,
        &server_id,
        &local_path,
        &remote_path,
        &transfer_id,
        file_index.unwrap_or(1),
        file_count.unwrap_or(1),
    )
    .await
    .map_err(|error| map_sftp_error("upload", &server_id, &remote_path, error))
}

#[tauri::command]
fn sftp_collect_local_files(
    directory: String,
) -> Result<Vec<sftp_browser::LocalFileEntry>, String> {
    sftp_browser::collect_local_files(&directory).map_err(|error| format!("{:#}", error))
}

#[tauri::command]
async fn sftp_disconnect(server_id: String) {
    sftp_browser::disconnect(&server_id).await;
}

#[tauri::command]
async fn sftp_download(
    state: State<'_, AppState>,
    server_id: String,
    remote_path: String,
    local_path: String,
) -> Result<(), String> {
    let config = state.config.read().await.clone();
    sftp_browser::download_file(&config, &server_id, &remote_path, &local_path)
        .await
        .map_err(|error| map_sftp_error("download", &server_id, &remote_path, error))
}

#[tauri::command]
async fn sftp_mkdir(
    state: State<'_, AppState>,
    server_id: String,
    path: String,
) -> Result<(), String> {
    let config = state.config.read().await.clone();
    sftp_browser::mkdir(&config, &server_id, &path)
        .await
        .map_err(|error| map_sftp_error("mkdir", &server_id, &path, error))
}

#[tauri::command]
async fn sftp_remove(
    state: State<'_, AppState>,
    server_id: String,
    path: String,
) -> Result<(), String> {
    let config = state.config.read().await.clone();
    sftp_browser::remove_path(&config, &server_id, &path)
        .await
        .map_err(|error| map_sftp_error("remove", &server_id, &path, error))
}

#[tauri::command]
async fn sftp_rename(
    state: State<'_, AppState>,
    server_id: String,
    from_path: String,
    to_path: String,
) -> Result<(), String> {
    let config = state.config.read().await.clone();
    sftp_browser::rename(&config, &server_id, &from_path, &to_path)
        .await
        .map_err(|error| {
            map_sftp_error("rename", &server_id, &format!("{} -> {}", from_path, to_path), error)
        })
}

#[tauri::command]
fn get_log_path() -> String {
    logger::current_log_path().to_string_lossy().to_string()
}

#[tauri::command]
fn get_log_dir() -> String {
    logger::log_dir().to_string_lossy().to_string()
}

#[tauri::command]
async fn set_logging_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    logger::set_enabled(enabled);
    let mut config = state.config.read().await.clone();
    config.logging.enabled = enabled;
    config::save(&config).map_err(|error| error.to_string())?;
    *state.config.write().await = config;
    if enabled {
        logger::append_log("诊断日志已启用");
    }
    Ok(())
}

#[tauri::command]
fn open_log_dir() -> Result<(), String> {
    logger::open_log_dir().map_err(|error| error.to_string())
}

#[tauri::command]
fn read_recent_logs(max_lines: Option<usize>) -> Result<String, String> {
    logger::read_recent_logs(max_lines.unwrap_or(200)).map_err(|error| error.to_string())
}

/// 找一个空闲的本地端口
fn free_local_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("获取空闲端口失败: {}", error))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
}

/// 启动系统远程桌面：用 mstsc /v 直连，避免未签名 .rdp 弹出「未知发布者」警告
#[cfg(target_os = "windows")]
fn launch_rdp_client(address: &str, width: Option<u32>, height: Option<u32>, fullscreen: bool) -> Result<(), String> {
    let mut command = std::process::Command::new("mstsc");
    command.arg(format!("/v:{}", address));
    if fullscreen {
        command.arg("/f");
    } else if let (Some(rdp_width), Some(rdp_height)) = (width, height) {
        command.arg(format!("/w:{}", rdp_width));
        command.arg(format!("/h:{}", rdp_height));
    }
    command
        .spawn()
        .map_err(|error| format!("启动 mstsc 失败: {}", error))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_rdp_client(address: &str, width: Option<u32>, height: Option<u32>, fullscreen: bool) -> Result<(), String> {
    // 需要安装 Microsoft Remote Desktop（新版名为 Windows App），从 App Store 免费获取
    let screen_mode = if fullscreen { 2 } else { 1 };
    let size_lines = match (width, height, fullscreen) {
        (_, _, true) => String::new(),
        (Some(rdp_width), Some(rdp_height), false) => {
            format!("desktopwidth:i:{rdp_width}\ndesktopheight:i:{rdp_height}\n")
        }
        _ => String::new(),
    };
    let rdp_content = format!(
        "full address:s:{address}\nprompt for credentials:i:1\nscreen mode id:i:{screen_mode}\n{size_lines}"
    );
    let file_name = format!("shhd-deploy-{}.rdp", address.replace([':', '.'], "-"));
    let rdp_path = std::env::temp_dir().join(file_name);
    std::fs::write(&rdp_path, rdp_content)
        .map_err(|error| format!("写入 .rdp 文件失败: {}", error))?;
    std::process::Command::new("open")
        .arg(&rdp_path)
        .spawn()
        .map_err(|error| {
            format!(
                "打开远程桌面失败: {}（请确认已安装 Microsoft Remote Desktop / Windows App）",
                error
            )
        })?;
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn launch_rdp_client(
    _address: &str,
    _width: Option<u32>,
    _height: Option<u32>,
    _fullscreen: bool,
) -> Result<(), String> {
    Err("当前系统暂不支持一键远程桌面".into())
}

/// 一键远程桌面：需要跳板时先建隧道再拉起 mstsc，直连则直接打开
#[tauri::command]
async fn open_rdp(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
    width: Option<u32>,
    height: Option<u32>,
    fullscreen: Option<bool>,
) -> Result<String, String> {
    let fullscreen = fullscreen.unwrap_or(false);
    let config = state.config.read().await.clone();
    let server = config
        .find_server(&server_id)
        .map_err(|error| error.to_string())?
        .clone();

    // 直连场景：不需要隧道
    let Some(_jump_id) = server.jump_server_id.clone() else {
        let address = format!("{}:3389", server.host);
        launch_rdp_client(&address, width, height, fullscreen)?;
        return Ok(address);
    };

    let tunnel_id = format!("rdp-auto-{}", server.id);
    let local_port = match state.tunnels.local_port_if_running(&tunnel_id).await {
        Some(port) => port,
        None => {
            let port = free_local_port()?;
            let tunnel_config = config::TunnelConfig {
                id: tunnel_id.clone(),
                name: format!("远程桌面-{}", server.name),
                via_server_id: server.jump_server_id.clone().unwrap_or_default(),
                local_port: port,
                remote_host: server.host.clone(),
                remote_port: 3389,
                auto_start: false,
                group: None,
            };
            state
                .tunnels
                .start_with_config(app, config.clone(), tunnel_config)
                .await
                .map_err(|error| format!("{:#}", error))?;
            state
                .tunnels
                .wait_active(&tunnel_id, std::time::Duration::from_secs(20))
                .await
                .map_err(|error| format!("{:#}", error))?;
            port
        }
    };

    let address = format!("127.0.0.1:{}", local_port);
    launch_rdp_client(&address, width, height, fullscreen)?;
    Ok(address)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_config = config::load_or_init().unwrap_or_default();
    logger::init(app_config.logging.enabled);
    if app_config.logging.enabled {
        logger::append_log(&format!(
            "应用启动 shhd-deploy v{}",
            env!("CARGO_PKG_VERSION")
        ));
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            config: Arc::new(RwLock::new(app_config)),
            tunnels: Arc::new(TunnelManager::default()),
            terminals: Arc::new(TerminalManager::default()),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            task_registry: Arc::new(TaskRegistry::default()),
            mcp: Arc::new(mcp::McpManager::default()),
        })
        .setup(|app| {
            let app_handle = app.handle().clone();
            let state: State<AppState> = app.state();
            let config_arc = state.config.clone();
            let tunnels = state.tunnels.clone();
            let mcp_manager = state.mcp.clone();
            tauri::async_runtime::spawn(async move {
                // 按配置启动 MCP 服务
                mcp_manager.apply(app_handle.clone()).await;
                // 自动启动标记了 autoStart 的隧道（跳过占位 host，避免无意义连接与前端报错）
                let config = config_arc.read().await.clone();
                for tunnel_config in config.tunnels.iter().filter(|tunnel| tunnel.auto_start) {
                    if config::is_placeholder_host(&tunnel_config.remote_host) {
                        eprintln!(
                            "跳过隧道「{}」自启动：远端地址未配置（{}）",
                            tunnel_config.name, tunnel_config.remote_host
                        );
                        continue;
                    }
                    if let Err(reason) =
                        config::validate_server_hosts(&config, &tunnel_config.via_server_id)
                    {
                        eprintln!(
                            "跳过隧道「{}」自启动：{}",
                            tunnel_config.name, reason
                        );
                        continue;
                    }
                    if let Err(error) = tunnels
                        .start(app_handle.clone(), config.clone(), &tunnel_config.id)
                        .await
                    {
                        eprintln!(
                            "隧道「{}」自启动失败: {:#}",
                            tunnel_config.name, error
                        );
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            get_config_path,
            export_config,
            import_config,
            test_server,
            test_server_draft,
            start_tunnel,
            stop_tunnel,
            tunnel_status,
            start_backend_deploy,
            start_rollback,
            get_releases,
            start_frontend_deploy,
            start_docker_deploy,
            cancel_task,
            terminal_open,
            terminal_write,
            terminal_resize,
            terminal_close,
            get_home_dir,
            list_local_drives,
            list_local_dir,
            sftp_list,
            sftp_upload,
            sftp_collect_local_files,
            sftp_disconnect,
            sftp_download,
            sftp_mkdir,
            sftp_remove,
            sftp_rename,
            open_rdp,
            get_mcp_status,
            get_log_path,
            get_log_dir,
            set_logging_enabled,
            open_log_dir,
            read_recent_logs
        ])
        .run(tauri::generate_context!())
        .expect("启动应用失败");
}
