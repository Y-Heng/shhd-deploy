//! 本地端口转发隧道：经跳板机把远端端口映射到 127.0.0.1，断线自动重连。

use crate::config::{self, AppConfig, TunnelConfig};
use crate::ssh::{self, SshConnection};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// 隧道状态快照（推送到前端）
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelStatusInfo {
    pub id: String,
    pub state: String,
    pub message: String,
    pub active_connections: u64,
    pub total_reconnects: u64,
}

struct TunnelHandle {
    cancel: CancellationToken,
    status: Arc<Mutex<TunnelStatusInfo>>,
    config: TunnelConfig,
}

/// 隧道管理器：负责所有隧道的启停与状态维护
#[derive(Default)]
pub struct TunnelManager {
    tunnels: Mutex<HashMap<String, TunnelHandle>>,
    /// 未进入运行循环时的静态状态（如配置校验失败）
    static_status: Mutex<HashMap<String, TunnelStatusInfo>>,
}

impl TunnelManager {
    /// 记录静态错误状态并推送到前端（不抛错、不启动连接）
    async fn report_static_status(
        &self,
        app: &AppHandle,
        tunnel_id: &str,
        state: &str,
        message: impl Into<String>,
    ) {
        let snapshot = TunnelStatusInfo {
            id: tunnel_id.to_string(),
            state: state.to_string(),
            message: message.into(),
            active_connections: 0,
            total_reconnects: 0,
        };
        self.static_status
            .lock()
            .await
            .insert(tunnel_id.to_string(), snapshot.clone());
        let _ = app.emit("tunnel-status", snapshot);
    }

    /// 按配置中的隧道 ID 启动
    pub async fn start(
        &self,
        app: AppHandle,
        config: AppConfig,
        tunnel_id: &str,
    ) -> Result<()> {
        let tunnel_config = config
            .tunnels
            .iter()
            .find(|tunnel| tunnel.id == tunnel_id)
            .with_context(|| format!("找不到隧道配置: {}", tunnel_id))?
            .clone();
        self.start_with_config(app, config, tunnel_config).await
    }

    /// 用任意隧道配置启动（支持临时隧道，如一键远程桌面）；已在运行则直接返回
    pub async fn start_with_config(
        &self,
        app: AppHandle,
        config: AppConfig,
        tunnel_config: TunnelConfig,
    ) -> Result<()> {
        {
            let mut map = self.tunnels.lock().await;
            if let Some(existing) = map.get(&tunnel_config.id) {
                if !existing.cancel.is_cancelled() {
                    return Ok(());
                }
                map.remove(&tunnel_config.id);
            }
        }

        if let Err(reason) = config::validate_server_hosts(&config, &tunnel_config.via_server_id) {
            self.report_static_status(&app, &tunnel_config.id, "error", reason)
                .await;
            return Ok(());
        }
        if config::is_placeholder_host(&tunnel_config.remote_host) {
            self.report_static_status(
                &app,
                &tunnel_config.id,
                "error",
                format!(
                    "远端地址未配置（{}）",
                    tunnel_config.remote_host.trim()
                ),
            )
            .await;
            return Ok(());
        }

        // 先绑定本地端口，端口冲突仅更新状态，不抛到前端弹窗
        let listener = match TcpListener::bind(("127.0.0.1", tunnel_config.local_port)).await {
            Ok(listener) => listener,
            Err(_) => {
                self.report_static_status(
                    &app,
                    &tunnel_config.id,
                    "error",
                    format!("本地端口 {} 被占用", tunnel_config.local_port),
                )
                .await;
                return Ok(());
            }
        };

        self.static_status.lock().await.remove(&tunnel_config.id);

        let cancel = CancellationToken::new();
        let status = Arc::new(Mutex::new(TunnelStatusInfo {
            id: tunnel_config.id.clone(),
            state: "connecting".into(),
            message: "正在连接".into(),
            active_connections: 0,
            total_reconnects: 0,
        }));

        {
            let mut map = self.tunnels.lock().await;
            map.insert(
                tunnel_config.id.clone(),
                TunnelHandle {
                    cancel: cancel.clone(),
                    status: status.clone(),
                    config: tunnel_config.clone(),
                },
            );
        }

        tokio::spawn(supervise_tunnel(
            app,
            config,
            tunnel_config,
            listener,
            cancel,
            status,
        ));
        Ok(())
    }

    /// 隧道正在运行时返回其本地端口（用于复用已建立的远程桌面隧道）
    pub async fn local_port_if_running(&self, tunnel_id: &str) -> Option<u16> {
        let map = self.tunnels.lock().await;
        let handle = map.get(tunnel_id)?;
        if handle.cancel.is_cancelled() {
            return None;
        }
        Some(handle.config.local_port)
    }

    /// 等待隧道进入 active 状态（超时报错）
    pub async fn wait_active(&self, tunnel_id: &str, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let state = {
                let map = self.tunnels.lock().await;
                match map.get(tunnel_id) {
                    Some(handle) => handle.status.lock().await.state.clone(),
                    None => "stopped".into(),
                }
            };
            match state.as_str() {
                "active" => return Ok(()),
                "stopped" | "error" => anyhow::bail!("隧道启动失败（状态: {}）", state),
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("等待隧道连接超时，请检查跳板机连通性");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// 停止隧道
    pub async fn stop(&self, tunnel_id: &str) {
        let map = self.tunnels.lock().await;
        if let Some(handle) = map.get(tunnel_id) {
            handle.cancel.cancel();
        }
        self.static_status.lock().await.remove(tunnel_id);
    }

    /// 获取所有已配置隧道的状态
    pub async fn status_all(&self, config: &AppConfig) -> Vec<TunnelStatusInfo> {
        let map = self.tunnels.lock().await;
        let static_status = self.static_status.lock().await;
        let mut result = Vec::new();
        for tunnel in &config.tunnels {
            if let Some(handle) = map.get(&tunnel.id) {
                result.push(handle.status.lock().await.clone());
            } else if let Some(status) = static_status.get(&tunnel.id) {
                result.push(status.clone());
            } else {
                result.push(TunnelStatusInfo {
                    id: tunnel.id.clone(),
                    state: "stopped".into(),
                    message: String::new(),
                    active_connections: 0,
                    total_reconnects: 0,
                });
            }
        }
        result
    }
}

async fn update_status(
    app: &AppHandle,
    status: &Arc<Mutex<TunnelStatusInfo>>,
    state: &str,
    message: String,
    active: u64,
    reconnects: u64,
) {
    let snapshot = {
        let mut guard = status.lock().await;
        guard.state = state.to_string();
        guard.message = message;
        guard.active_connections = active;
        guard.total_reconnects = reconnects;
        guard.clone()
    };
    let _ = app.emit("tunnel-status", snapshot);
}

/// 隧道守护任务：会话断开自动重连（指数退避，上限 30 秒）
async fn supervise_tunnel(
    app: AppHandle,
    config: AppConfig,
    tunnel: TunnelConfig,
    listener: TcpListener,
    cancel: CancellationToken,
    status: Arc<Mutex<TunnelStatusInfo>>,
) {
    let active_counter = Arc::new(AtomicU64::new(0));
    let mut reconnects: u64 = 0;
    let mut backoff_secs: u64 = 2;

    'outer: loop {
        if cancel.is_cancelled() {
            break;
        }
        update_status(
            &app,
            &status,
            "connecting",
            format!("正在连接经由服务器 {}", tunnel.via_server_id),
            active_counter.load(Ordering::Relaxed),
            reconnects,
        )
        .await;

        let connection = tokio::select! {
            _ = cancel.cancelled() => break 'outer,
            result = ssh::connect(&config, &tunnel.via_server_id) => result,
        };

        let connection = match connection {
            Ok(conn) => Arc::new(conn),
            Err(error) => {
                reconnects += 1;
                update_status(
                    &app,
                    &status,
                    "reconnecting",
                    format!("连接失败: {}，{} 秒后重试", error, backoff_secs),
                    active_counter.load(Ordering::Relaxed),
                    reconnects,
                )
                .await;
                tokio::select! {
                    _ = cancel.cancelled() => break 'outer,
                    _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
                }
                backoff_secs = (backoff_secs * 2).min(30);
                continue;
            }
        };

        backoff_secs = 2;
        update_status(
            &app,
            &status,
            "active",
            format!(
                "127.0.0.1:{} -> {}:{}",
                tunnel.local_port, tunnel.remote_host, tunnel.remote_port
            ),
            active_counter.load(Ordering::Relaxed),
            reconnects,
        )
        .await;

        // 健康检查放宽间隔，减少状态推送抢占转发任务
        let mut health_interval = tokio::time::interval(Duration::from_secs(15));
        health_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_reported_active = u64::MAX;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break 'outer,
                _ = health_interval.tick() => {
                    if connection.is_closed() {
                        reconnects += 1;
                        update_status(
                            &app,
                            &status,
                            "reconnecting",
                            "SSH 会话断开，准备重连".into(),
                            active_counter.load(Ordering::Relaxed),
                            reconnects,
                        )
                        .await;
                        break;
                    }
                    let active_now = active_counter.load(Ordering::Relaxed);
                    // 活跃连接数变化时才推送，避免无意义前端刷新
                    if active_now != last_reported_active {
                        last_reported_active = active_now;
                        update_status(
                            &app,
                            &status,
                            "active",
                            format!(
                                "127.0.0.1:{} -> {}:{}",
                                tunnel.local_port, tunnel.remote_host, tunnel.remote_port
                            ),
                            active_now,
                            reconnects,
                        )
                        .await;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((tcp_stream, _peer)) => {
                            tokio::spawn(forward_connection(
                                connection.clone(),
                                tunnel.clone(),
                                tcp_stream,
                                active_counter.clone(),
                            ));
                        }
                        Err(_) => {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                    }
                }
            }
        }
    }

    update_status(
        &app,
        &status,
        "stopped",
        "已停止".into(),
        0,
        reconnects,
    )
    .await;
}

/// 单条客户端连接的转发：本地 TCP <-> SSH direct-tcpip 通道
async fn forward_connection(
    connection: Arc<SshConnection>,
    tunnel: TunnelConfig,
    mut tcp_stream: TcpStream,
    active_counter: Arc<AtomicU64>,
) {
    // 关闭 Nagle，避免 RDP 小包被合并导致体感卡顿
    let _ = tcp_stream.set_nodelay(true);

    let channel = match connection
        .handle
        .channel_open_direct_tcpip(
            &tunnel.remote_host,
            tunnel.remote_port as u32,
            "127.0.0.1",
            tunnel.local_port as u32,
        )
        .await
    {
        Ok(channel) => channel,
        Err(_) => return,
    };

    active_counter.fetch_add(1, Ordering::Relaxed);
    let mut channel_stream = channel.into_stream();
    // 大缓冲双向拷贝，提升吞吐量（默认 8KB 对 RDP 偏小）
    const FORWARD_BUF: usize = 128 * 1024;
    let _ = tokio::io::copy_bidirectional_with_sizes(
        &mut tcp_stream,
        &mut channel_stream,
        FORWARD_BUF,
        FORWARD_BUF,
    )
    .await;
    active_counter.fetch_sub(1, Ordering::Relaxed);
}
