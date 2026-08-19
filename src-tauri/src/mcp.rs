use crate::config::{AppConfig, McpPermission};
use crate::deploy_backend::{self, BackendDeployRequest, DeployMode};
use crate::deploy_frontend::FrontendDeployOptions;
use crate::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// MCP 服务管理器：随配置启停 HTTP 服务
#[derive(Default)]
pub struct McpManager {
    running: Mutex<Option<(u16, CancellationToken)>>,
}

impl McpManager {
    /// 按当前配置应用启停（端口变化会重启；权限变化即时生效无需重启）
    pub async fn apply(&self, app: AppHandle) {
        let mcp_config = {
            let state: tauri::State<AppState> = app.state();
            let config = state.config.read().await;
            config.mcp.clone()
        };
        let mut guard = self.running.lock().await;
        if let Some((port, token)) = guard.as_ref() {
            if mcp_config.enabled && *port == mcp_config.port {
                return;
            }
            token.cancel();
            *guard = None;
        }
        if !mcp_config.enabled {
            return;
        }
        let token = CancellationToken::new();
        match start_server(app, mcp_config.port, token.clone()).await {
            Ok(()) => *guard = Some((mcp_config.port, token)),
            Err(error) => eprintln!("MCP 服务启动失败: {}", error),
        }
    }

    /// 运行中返回监听端口
    pub async fn running_port(&self) -> Option<u16> {
        self.running.lock().await.as_ref().map(|(port, _)| *port)
    }
}

async fn start_server(app: AppHandle, port: u16, token: CancellationToken) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| anyhow::anyhow!("绑定端口 {} 失败: {}", port, error))?;
    let router = Router::new()
        .route("/mcp", post(handle_mcp_post).get(handle_mcp_get))
        .with_state(app);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(token.cancelled_owned())
            .await;
    });
    Ok(())
}

/// GET /mcp：本实现不使用服务器主动推送
async fn handle_mcp_get() -> impl IntoResponse {
    StatusCode::METHOD_NOT_ALLOWED
}

struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// MCP Streamable HTTP 入口：处理 JSON-RPC 消息
async fn handle_mcp_post(
    State(app): State<AppHandle>,
    Json(message): Json<Value>,
) -> axum::response::Response {
    // 通知消息（无 id）直接确认
    let Some(request_id) = message.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let method = message
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    let body = match dispatch(&app, &method, params).await {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": request_id, "result": result }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": { "code": error.code, "message": error.message }
        }),
    };
    Json(body).into_response()
}

async fn dispatch(app: &AppHandle, method: &str, params: Value) -> Result<Value, RpcError> {
    match method {
        "initialize" => {
            let protocol_version = params
                .get("protocolVersion")
                .and_then(|value| value.as_str())
                .unwrap_or("2025-03-26")
                .to_string();
            Ok(json!({
                "protocolVersion": protocol_version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "shhd-deploy", "version": "0.1.0" },
                "instructions": "部署工具 MCP 服务。典型流程：1) list_config 查看可部署目标；2) 本地构建/发布产物；3) backend_deploy 或 frontend_deploy 发起部署（mode=stage 仅上传中转，不动线上）；4) get_task_status 轮询任务进度直到 success/failed。部署工具返回 taskId，务必轮询确认结果。"
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => {
            let permission = current_permission(app).await;
            Ok(json!({ "tools": tool_definitions(permission) }))
        }
        "tools/call" => {
            let tool_name = params
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(app, &tool_name, arguments).await {
                Ok(text) => Ok(json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false
                })),
                Err(message) => Ok(json!({
                    "content": [{ "type": "text", "text": message }],
                    "isError": true
                })),
            }
        }
        _ => Err(RpcError::new(-32601, format!("不支持的方法: {}", method))),
    }
}

async fn current_permission(app: &AppHandle) -> McpPermission {
    let state: tauri::State<AppState> = app.state();
    let config = state.config.read().await;
    config.mcp.permission
}

/// 按权限级别返回可用工具清单
fn tool_definitions(permission: McpPermission) -> Vec<Value> {
    let mut tools = vec![
        json!({
            "name": "list_config",
            "description": "查看可部署目标：后端负载组及项目、前端部署目标、Docker 目标、隧道（不含任何密码凭据）。发起部署前先调用它获取 id。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "list_releases",
            "description": "查看最近的后端发布历史，包含待替换(staged)、成功(success)、已回滚(rolled_back)、回滚完成(rollback)、失败(failed)状态。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "list_frontend_releases",
            "description": "查看最近的前端发布历史。status 为 success 且带 backupSuffix 的记录可回滚。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "get_task_status",
            "description": "查询部署任务状态与日志。waitSeconds>0 时会阻塞等待任务结束或超时（最长 300 秒），建议部署后用 waitSeconds=60 轮询。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "taskId": { "type": "string", "description": "部署工具返回的任务 ID" },
                    "waitSeconds": { "type": "integer", "description": "等待秒数，0 表示立即返回", "default": 0 }
                },
                "required": ["taskId"]
            }
        }),
        json!({
            "name": "list_tunnels",
            "description": "查看所有隧道配置与当前运行状态。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ];

    if permission != McpPermission::Readonly {
        let mode_hint = if permission == McpPermission::Stage {
            "当前权限为「仅中转」，mode 只能是 stage（上传到中转目录，不影响线上）。"
        } else {
            "mode 可选 full(上传并立即替换)/stage(仅上传中转)/replace(用已中转内容替换线上)。"
        };
        tools.push(json!({
            "name": "backend_deploy",
            "description": format!("后端部署：把本地发布产物部署到 Windows 负载组。{}", mode_hint),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "groupId": { "type": "string", "description": "负载组 id（list_config 获取）" },
                    "projectIds": { "type": "array", "items": { "type": "string" }, "description": "项目 id 列表，缺省为组内全部项目" },
                    "releaseName": { "type": "string", "description": "发布名称，格式 yyyyMMdd-功能名，如 20260812-优惠券功能；replace 模式填已中转的发布名" },
                    "mode": { "type": "string", "enum": ["full", "stage", "replace"], "default": "stage" },
                    "backupSibling": { "type": "boolean", "description": "替换前把应用目录备份为 目录名-日期", "default": true }
                },
                "required": ["groupId", "releaseName"]
            }
        }));
        tools.push(json!({
            "name": "frontend_deploy",
            "description": format!("前端部署：把本地构建产物打包上传到 nginx 服务器后解压。{}", mode_hint),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "targetIds": { "type": "array", "items": { "type": "string" }, "description": "前端目标 id 列表（list_config 获取）" },
                    "mode": { "type": "string", "enum": ["full", "stage", "replace"], "default": "stage" },
                    "backupSibling": { "type": "boolean", "description": "替换前备份线上目录，供回滚", "default": true }
                },
                "required": ["targetIds"]
            }
        }));
    }

    if permission == McpPermission::Full {
        tools.push(json!({
            "name": "rollback",
            "description": "回滚一次后端发布：停止 IIS 后恢复替换前备份的 bin 并做健康检查。releaseId 从 list_releases 获取（仅 success 状态可回滚）。",
            "inputSchema": {
                "type": "object",
                "properties": { "releaseId": { "type": "string" } },
                "required": ["releaseId"]
            }
        }));
        tools.push(json!({
            "name": "frontend_rollback",
            "description": "回滚一次前端发布：把线上静态目录恢复为该次发布前的快照。releaseId 从 list_frontend_releases 获取（仅 success 且带 backupSuffix 可回滚）。",
            "inputSchema": {
                "type": "object",
                "properties": { "releaseId": { "type": "string" } },
                "required": ["releaseId"]
            }
        }));
        tools.push(json!({
            "name": "docker_deploy",
            "description": "在 Linux 服务器上按顺序执行 Docker 目标配置的命令（如 compose pull/up）。",
            "inputSchema": {
                "type": "object",
                "properties": { "targetId": { "type": "string" } },
                "required": ["targetId"]
            }
        }));
        tools.push(json!({
            "name": "tunnel_control",
            "description": "启动或停止一条隧道。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tunnelId": { "type": "string" },
                    "action": { "type": "string", "enum": ["start", "stop"] }
                },
                "required": ["tunnelId", "action"]
            }
        }));
    }

    tools
}

fn arg_str(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(|value| value.as_str())
        .map(|text| text.to_string())
}

fn arg_str_list(arguments: &Value, key: &str) -> Vec<String> {
    arguments
        .get(key)
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(|text| text.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// 目标是否在白名单内（null = 全部允许）
fn is_allowed(allowlist: &Option<Vec<String>>, id: &str) -> bool {
    match allowlist {
        None => true,
        Some(ids) => ids.iter().any(|allowed| allowed == id),
    }
}

async fn call_tool(app: &AppHandle, tool_name: &str, arguments: Value) -> Result<String, String> {
    let (config, permission) = {
        let state: tauri::State<AppState> = app.state();
        let config = state.config.read().await.clone();
        let permission = config.mcp.permission;
        (config, permission)
    };

    match tool_name {
        "list_config" => Ok(build_config_summary(&config)),
        "list_releases" => {
            let releases = deploy_backend::load_releases();
            let recent: Vec<Value> = releases
                .iter()
                .take(20)
                .map(|record| {
                    json!({
                        "releaseId": record.id,
                        "releaseName": record.release_name,
                        "groupId": record.group_id,
                        "groupName": record.group_name,
                        "projectIds": record.project_ids,
                        "createdAt": record.created_at,
                        "status": record.status,
                    })
                })
                .collect();
            Ok(serde_json::to_string_pretty(&recent).unwrap_or_default())
        }
        "list_frontend_releases" => {
            let releases = crate::deploy_frontend::load_frontend_releases();
            let recent: Vec<Value> = releases
                .iter()
                .take(20)
                .map(|record| {
                    json!({
                        "releaseId": record.id,
                        "createdAt": record.created_at,
                        "mode": record.mode,
                        "groupName": record.group_name,
                        "targetIds": record.target_ids,
                        "targetNames": record.target_names,
                        "status": record.status,
                        "backupSuffix": record.backup_suffix,
                        "message": record.message,
                    })
                })
                .collect();
            Ok(serde_json::to_string_pretty(&recent).unwrap_or_default())
        }
        "get_task_status" => {
            let task_id = arg_str(&arguments, "taskId").ok_or("缺少参数 taskId")?;
            let wait_seconds = arguments
                .get("waitSeconds")
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
                .min(300);
            let state: tauri::State<AppState> = app.state();
            let registry = state.task_registry.clone();
            let deadline =
                tokio::time::Instant::now() + std::time::Duration::from_secs(wait_seconds);
            loop {
                let Some(snapshot) = registry.snapshot(&task_id) else {
                    return Err(format!("找不到任务: {}", task_id));
                };
                let is_terminal = snapshot.state != "running";
                if is_terminal || tokio::time::Instant::now() >= deadline {
                    let log_tail: Vec<&String> = snapshot
                        .logs
                        .iter()
                        .rev()
                        .take(80)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    return Ok(serde_json::to_string_pretty(&json!({
                        "taskId": task_id,
                        "state": snapshot.state,
                        "message": snapshot.message,
                        "percent": snapshot.percent,
                        "step": snapshot.step,
                        "logs": log_tail,
                    }))
                    .unwrap_or_default());
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        "list_tunnels" => {
            let state: tauri::State<AppState> = app.state();
            let status_list = state.tunnels.status_all(&config).await;
            let items: Vec<Value> = config
                .tunnels
                .iter()
                .map(|tunnel| {
                    let status = status_list.iter().find(|status| status.id == tunnel.id);
                    json!({
                        "tunnelId": tunnel.id,
                        "name": tunnel.name,
                        "localPort": tunnel.local_port,
                        "remote": format!("{}:{}", tunnel.remote_host, tunnel.remote_port),
                        "state": status.map(|item| item.state.clone()).unwrap_or_else(|| "stopped".into()),
                    })
                })
                .collect();
            Ok(serde_json::to_string_pretty(&items).unwrap_or_default())
        }
        "backend_deploy" => {
            if permission == McpPermission::Readonly {
                return Err("当前 MCP 权限为只读，不允许发起部署".into());
            }
            let group_id = arg_str(&arguments, "groupId").ok_or("缺少参数 groupId")?;
            let release_name = arg_str(&arguments, "releaseName").ok_or("缺少参数 releaseName")?;
            if !is_allowed(&config.mcp.allowed_backend_group_ids, &group_id) {
                return Err(format!("负载组 {} 不在 MCP 允许访问的范围内", group_id));
            }
            let group = config
                .backend_groups
                .iter()
                .find(|group| group.id == group_id)
                .ok_or(format!("找不到负载组: {}", group_id))?;

            let mode: DeployMode = arguments
                .get("mode")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| "mode 参数无效，可选 full/stage/replace")?
                .unwrap_or(DeployMode::Stage);
            if permission == McpPermission::Stage && mode != DeployMode::Stage {
                return Err(
                    "当前 MCP 权限为「仅中转」，mode 只能是 stage；替换线上请在软件界面操作或调整权限"
                        .into(),
                );
            }
            let mut project_ids = arg_str_list(&arguments, "projectIds");
            if project_ids.is_empty() {
                project_ids = group.projects.iter().map(|project| project.id.clone()).collect();
            }
            let backup_sibling = arguments
                .get("backupSibling")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);

            let request = BackendDeployRequest {
                group_id,
                project_ids,
                release_name,
                copy_mode: None,
                mode,
                backup_sibling,
                preview_paths: Default::default(),
                newer_than: Some(chrono::Local::now().format("%Y-%m-%d").to_string()),
            };
            let task_id = crate::launch_backend_deploy(app, request).await;
            Ok(format!(
                "部署任务已启动，taskId: {}。请调用 get_task_status（建议 waitSeconds=60）轮询直到 state 为 success/failed。",
                task_id
            ))
        }
        "frontend_deploy" => {
            if permission == McpPermission::Readonly {
                return Err("当前 MCP 权限为只读，不允许发起部署".into());
            }
            let target_ids = arg_str_list(&arguments, "targetIds");
            if target_ids.is_empty() {
                return Err("缺少参数 targetIds".into());
            }
            for target_id in &target_ids {
                if !is_allowed(&config.mcp.allowed_frontend_target_ids, target_id) {
                    return Err(format!("前端目标 {} 不在 MCP 允许访问的范围内", target_id));
                }
            }
            let mode: DeployMode = arguments
                .get("mode")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| "mode 参数无效，可选 full/stage/replace")?
                .unwrap_or(DeployMode::Stage);
            if permission == McpPermission::Stage && mode != DeployMode::Stage {
                return Err(
                    "当前 MCP 权限为「仅中转」，mode 只能是 stage；替换线上请在软件界面操作或调整权限"
                        .into(),
                );
            }
            let backup_sibling = arguments
                .get("backupSibling")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let options = FrontendDeployOptions {
                mode,
                backup_sibling,
            };
            let task_id = crate::launch_frontend_deploy(app, target_ids, options).await;
            Ok(format!(
                "前端部署任务已启动，taskId: {}。请调用 get_task_status 轮询结果。",
                task_id
            ))
        }
        "rollback" => {
            if permission != McpPermission::Full {
                return Err("回滚需要 MCP「完全访问」权限".into());
            }
            let release_id = arg_str(&arguments, "releaseId").ok_or("缺少参数 releaseId")?;
            let task_id = crate::launch_rollback(app, release_id).await;
            Ok(format!(
                "回滚任务已启动，taskId: {}。请调用 get_task_status 轮询结果。",
                task_id
            ))
        }
        "frontend_rollback" => {
            if permission != McpPermission::Full {
                return Err("回滚需要 MCP「完全访问」权限".into());
            }
            let release_id = arg_str(&arguments, "releaseId").ok_or("缺少参数 releaseId")?;
            let task_id = crate::launch_frontend_rollback(app, release_id).await;
            Ok(format!(
                "前端回滚任务已启动，taskId: {}。请调用 get_task_status 轮询结果。",
                task_id
            ))
        }
        "docker_deploy" => {
            if permission != McpPermission::Full {
                return Err("Docker 部署需要 MCP「完全访问」权限".into());
            }
            let target_id = arg_str(&arguments, "targetId").ok_or("缺少参数 targetId")?;
            if !is_allowed(&config.mcp.allowed_docker_target_ids, &target_id) {
                return Err(format!("Docker 目标 {} 不在 MCP 允许访问的范围内", target_id));
            }
            let task_id = crate::launch_docker_deploy(app, target_id).await;
            Ok(format!(
                "Docker 部署任务已启动，taskId: {}。请调用 get_task_status 轮询结果。",
                task_id
            ))
        }
        "tunnel_control" => {
            if permission != McpPermission::Full {
                return Err("隧道控制需要 MCP「完全访问」权限".into());
            }
            let tunnel_id = arg_str(&arguments, "tunnelId").ok_or("缺少参数 tunnelId")?;
            let action = arg_str(&arguments, "action").ok_or("缺少参数 action")?;
            let state: tauri::State<AppState> = app.state();
            match action.as_str() {
                "start" => {
                    state
                        .tunnels
                        .start(app.clone(), config, &tunnel_id)
                        .await
                        .map_err(|error| format!("{:#}", error))?;
                    Ok(format!("隧道 {} 已启动", tunnel_id))
                }
                "stop" => {
                    state.tunnels.stop(&tunnel_id).await;
                    Ok(format!("隧道 {} 已停止", tunnel_id))
                }
                _ => Err("action 只能是 start 或 stop".into()),
            }
        }
        _ => Err(format!("未知工具: {}", tool_name)),
    }
}

/// 构建脱敏的配置摘要（不含密码/密钥信息），并按白名单过滤
fn build_config_summary(config: &AppConfig) -> String {
    let servers: Vec<Value> = config
        .servers
        .iter()
        .map(|server| {
            json!({
                "serverId": server.id,
                "name": server.name,
                "os": server.os,
                "host": server.host,
                "group": server.group,
            })
        })
        .collect();

    let backend_groups: Vec<Value> = config
        .backend_groups
        .iter()
        .filter(|group| is_allowed(&config.mcp.allowed_backend_group_ids, &group.id))
        .map(|group| {
            json!({
                "groupId": group.id,
                "name": group.name,
                "serverIds": group.effective_server_ids(),
                "stagingDir": group.staging_dir,
                "projects": group.projects.iter().map(|project| json!({
                    "projectId": project.id,
                    "name": project.name,
                    "localBinDir": project.local_bin_dir,
                    "remoteAppDir": project.remote_app_dir,
                    "newerThan": project.newer_than,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    let frontend_targets: Vec<Value> = config
        .frontend_targets
        .iter()
        .filter(|target| is_allowed(&config.mcp.allowed_frontend_target_ids, &target.id))
        .map(|target| {
            json!({
                "targetId": target.id,
                "name": target.name,
                "localDir": target.local_dir,
                "remoteDir": target.remote_dir,
                "serverIds": target.server_ids,
            })
        })
        .collect();

    let docker_targets: Vec<Value> = config
        .docker_targets
        .iter()
        .filter(|target| is_allowed(&config.mcp.allowed_docker_target_ids, &target.id))
        .map(|target| {
            json!({
                "targetId": target.id,
                "name": target.name,
                "serverId": target.server_id,
                "workDir": target.work_dir,
                "commands": target.commands,
            })
        })
        .collect();

    serde_json::to_string_pretty(&json!({
        "permission": config.mcp.permission,
        "backendGroups": backend_groups,
        "frontendTargets": frontend_targets,
        "dockerTargets": docker_targets,
        "servers": servers,
        "hint": "backend_deploy 的 releaseName 格式为 yyyyMMdd-功能名；本地产物目录（localBinDir/localDir）需要先由构建流程产出最新内容再发起部署。",
    }))
    .unwrap_or_default()
}
