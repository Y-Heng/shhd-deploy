use crate::config::{AppConfig, OsType};
use crate::ssh;
use anyhow::{Context, Result};
use base64::Engine;
use russh::{ChannelMsg, Pty};
use serde::Serialize;
use std::collections::HashMap;
use std::time::Instant;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Mutex};

/// 交互式 PTY 终端模式。OCRNL 必须为 0，否则 PowerShell 用 CR 重绘提示符时会变成换行。
/// Windows ConPTY 本身已输出 CRLF，ONLCR 再开会空一行。
fn interactive_pty_modes(windows: bool) -> Vec<(Pty, u32)> {
    let onlcr = if windows { 0 } else { 1 };
    vec![
        (Pty::VINTR, 3),
        (Pty::VQUIT, 28),
        (Pty::VERASE, 127),
        (Pty::VKILL, 21),
        (Pty::VEOF, 4),
        (Pty::ICRNL, 1),
        (Pty::IGNCR, 0),
        (Pty::INLCR, 0),
        (Pty::ISIG, 1),
        (Pty::ICANON, 1),
        (Pty::ECHO, 1),
        (Pty::ECHOE, 1),
        (Pty::ECHOK, 1),
        (Pty::ECHONL, 0),
        (Pty::OPOST, 1),
        (Pty::ONLCR, onlcr),
        (Pty::OCRNL, 0),
        (Pty::ONOCR, 0),
        (Pty::ONLRET, 0),
        (Pty::CS8, 1),
        (Pty::IUTF8, 1),
        (Pty::TTY_OP_ISPEED, 38400),
        (Pty::TTY_OP_OSPEED, 38400),
    ]
}

/// 发送给终端会话任务的控制命令
enum TermCommand {
    /// 用户键入的数据
    Write(Vec<u8>),
    /// 调整终端窗口大小（列, 行）
    Resize(u32, u32),
    /// 关闭会话
    Close,
}

/// 终端输出事件负载（data 为 base64 编码的原始字节）
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TermDataPayload {
    session_id: String,
    data: String,
}

/// 终端关闭事件负载
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TermClosedPayload {
    session_id: String,
}

/// SSH 终端会话管理器
#[derive(Default)]
pub struct TerminalManager {
    sessions: Mutex<HashMap<String, mpsc::Sender<TermCommand>>>,
}

impl TerminalManager {
    /// 打开一个交互式 shell 会话，返回会话 ID
    pub async fn open(
        &self,
        app: AppHandle,
        config: AppConfig,
        server_id: &str,
        cols: u32,
        rows: u32,
    ) -> Result<String> {
        let started = Instant::now();
        let conn = ssh::connect(&config, server_id).await?;
        crate::logger::append_log(&format!(
            "terminal 握手 [{}] {} ms",
            server_id,
            started.elapsed().as_millis()
        ));
        let channel = conn.handle.channel_open_session().await?;
        channel
            .request_pty(
                true,
                "xterm-256color",
                cols.max(20),
                rows.max(5),
                0,
                0,
                &interactive_pty_modes(conn.server.os == OsType::Windows),
            )
            .await
            .context("申请 PTY 失败")?;
        channel.request_shell(true).await.context("启动 shell 失败")?;

        // Windows 必须走 login shell（exec 配 ConPTY 会空白）。
        // 不要 chcp 65001 / 改 OutputEncoding：会让 PS 5.1 多打空行、光标错乱。
        if conn.server.os == OsType::Windows {
            let _ = channel.data(&b"powershell -NoLogo -NoProfile -NoExit -Command \"Remove-Module PSReadLine -ErrorAction SilentlyContinue\"\r\n"[..]).await;
        }
        crate::logger::append_log(&format!(
            "terminal 会话就绪 [{}] {} ms",
            server_id,
            started.elapsed().as_millis()
        ));

        let session_id = uuid::Uuid::new_v4().to_string();
        let (command_tx, mut command_rx) = mpsc::channel::<TermCommand>(64);
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), command_tx);

        let task_session_id = session_id.clone();
        tokio::spawn(async move {
            // conn 被移入任务持有，保证跳板链存活
            let _conn = conn;
            let mut channel = channel;
            loop {
                tokio::select! {
                    message = channel.wait() => {
                        match message {
                            Some(ChannelMsg::Data { ref data }) => {
                                let payload = TermDataPayload {
                                    session_id: task_session_id.clone(),
                                    data: base64::engine::general_purpose::STANDARD.encode(data),
                                };
                                let _ = app.emit("term-data", payload);
                            }
                            Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                                let payload = TermDataPayload {
                                    session_id: task_session_id.clone(),
                                    data: base64::engine::general_purpose::STANDARD.encode(data),
                                };
                                let _ = app.emit("term-data", payload);
                            }
                            Some(ChannelMsg::ExitStatus { .. }) | Some(ChannelMsg::Close) | None => break,
                            _ => {}
                        }
                    }
                    command = command_rx.recv() => {
                        match command {
                            Some(TermCommand::Write(bytes)) => {
                                if channel.data(&bytes[..]).await.is_err() { break; }
                            }
                            Some(TermCommand::Resize(new_cols, new_rows)) => {
                                let _ = channel.window_change(new_cols, new_rows, 0, 0).await;
                            }
                            Some(TermCommand::Close) | None => {
                                let _ = channel.close().await;
                                break;
                            }
                        }
                    }
                }
            }
            let _ = app.emit(
                "term-closed",
                TermClosedPayload {
                    session_id: task_session_id.clone(),
                },
            );
        });

        Ok(session_id)
    }

    pub async fn write(&self, session_id: &str, data: &str) {
        let sender = { self.sessions.lock().await.get(session_id).cloned() };
        if let Some(sender) = sender {
            let _ = sender
                .send(TermCommand::Write(data.as_bytes().to_vec()))
                .await;
        }
    }

    pub async fn resize(&self, session_id: &str, cols: u32, rows: u32) {
        let sender = { self.sessions.lock().await.get(session_id).cloned() };
        if let Some(sender) = sender {
            let _ = sender.send(TermCommand::Resize(cols, rows)).await;
        }
    }

    pub async fn close(&self, session_id: &str) {
        let sender = { self.sessions.lock().await.remove(session_id) };
        if let Some(sender) = sender {
            let _ = sender.send(TermCommand::Close).await;
        }
    }
}
