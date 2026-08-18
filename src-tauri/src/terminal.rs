use crate::config::{AppConfig, OsType};
use crate::ssh;
use anyhow::{Context, Result};
use base64::Engine;
use russh::ChannelMsg;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Instant;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Mutex};

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
            .request_pty(true, "xterm-256color", cols.max(20), rows.max(5), 0, 0, &[])
            .await
            .context("申请 PTY 失败")?;

        if conn.server.os == OsType::Windows {
            // 强制 cmd：跳过可能被设成 powershell 的 DefaultShell，/d 跳过 AutoRun
            channel
                .exec(true, "cmd.exe /d /q /k chcp 65001>nul")
                .await
                .context("启动 Windows cmd 失败")?;
        } else {
            channel.request_shell(true).await.context("启动 shell 失败")?;
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
