use crate::config::{AppConfig, OsType};
use crate::ssh;
use anyhow::{Context, Result};
use base64::Engine;
use russh::{ChannelMsg, Pty};
use serde::Serialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};
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

/// 启动 PowerShell：无引号、无 -Command，避免旧版 OpenSSH 截断后卡在 stdin。
const START_POWERSHELL: &[u8] = b"powershell -NoLogo -NoProfile -NoExit\r";
/// 关掉 PSReadLine（跳板机下会闪烁/逐字换行），再 cls 清掉 ConPTY 顶部空行。
const PREPARE_POWERSHELL: &[u8] = b"Remove-Module PSReadLine -ErrorAction SilentlyContinue; cls\r";
const CLEAR_SCREEN: &[u8] = b"cls\r";

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WinPrompt {
    Cmd,
    PowerShell,
}

enum WinBootPhase {
    WaitPrompt,
    WaitPowerShell,
    Done,
}

/// Windows login shell 仍是 cmd：等提示符再切 PowerShell，避免命令灌进未就绪的控制台。
struct WinBoot {
    phase: WinBootPhase,
    buf: Vec<u8>,
    deadline: Instant,
}

impl WinBoot {
    fn new() -> Self {
        Self {
            phase: WinBootPhase::WaitPrompt,
            buf: Vec::new(),
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }

    fn deadline(&self) -> Option<Instant> {
        match self.phase {
            WinBootPhase::Done => None,
            _ => Some(self.deadline),
        }
    }

    fn on_output(&mut self, data: &[u8]) -> Option<&'static [u8]> {
        if matches!(self.phase, WinBootPhase::Done) {
            return None;
        }
        self.buf.extend_from_slice(data);
        if self.buf.len() > 4096 {
            let drain_to = self.buf.len() - 2048;
            self.buf.drain(..drain_to);
        }
        match self.phase {
            WinBootPhase::WaitPrompt => match detect_windows_prompt(&self.buf) {
                Some(WinPrompt::PowerShell) => self.enter_powershell_ready(),
                Some(WinPrompt::Cmd) => self.launch_powershell(),
                None => None,
            },
            WinBootPhase::WaitPowerShell => {
                if detect_windows_prompt(&self.buf) == Some(WinPrompt::PowerShell) {
                    self.enter_powershell_ready()
                } else {
                    None
                }
            }
            WinBootPhase::Done => None,
        }
    }

    fn on_timeout(&mut self) -> Option<&'static [u8]> {
        match self.phase {
            WinBootPhase::WaitPrompt => self.launch_powershell(),
            WinBootPhase::WaitPowerShell => {
                self.phase = WinBootPhase::Done;
                self.buf.clear();
                Some(CLEAR_SCREEN)
            }
            WinBootPhase::Done => None,
        }
    }

    fn launch_powershell(&mut self) -> Option<&'static [u8]> {
        self.phase = WinBootPhase::WaitPowerShell;
        self.deadline = Instant::now() + Duration::from_secs(12);
        self.buf.clear();
        crate::logger::append_log("terminal Windows 等待 PowerShell 启动");
        Some(START_POWERSHELL)
    }

    fn enter_powershell_ready(&mut self) -> Option<&'static [u8]> {
        self.phase = WinBootPhase::Done;
        self.buf.clear();
        crate::logger::append_log("terminal Windows 已进入 PowerShell");
        Some(PREPARE_POWERSHELL)
    }
}

fn last_nonempty_line(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    while end > 0 {
        let byte = buf[end - 1];
        if byte == b'\n' || byte == b'\r' || byte == b' ' || byte == 0 {
            end -= 1;
            continue;
        }
        break;
    }
    let start = buf[..end]
        .iter()
        .rposition(|&byte| byte == b'\n' || byte == b'\r')
        .map(|index| index + 1)
        .unwrap_or(0);
    &buf[start..end]
}

fn line_has_ascii(line: &[u8], needle: &[u8]) -> bool {
    line.windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn is_powershell_prompt(line: &[u8]) -> bool {
    if !line.ends_with(&[b'>']) {
        return false;
    }
    line.starts_with(b"PS>") || line.starts_with(b"PS ") || line.windows(4).any(|window| window == b" PS ")
}

fn detect_windows_prompt(buf: &[u8]) -> Option<WinPrompt> {
    let line = last_nonempty_line(buf);
    if line.is_empty() || !line.ends_with(&[b'>']) {
        return None;
    }
    // 正在回显我们注入的命令时不要当成提示符
    if line_has_ascii(line, b"powershell") || line_has_ascii(line, b"remove-module") || line_has_ascii(line, b"cls") {
        return None;
    }
    if is_powershell_prompt(line) {
        return Some(WinPrompt::PowerShell);
    }
    Some(WinPrompt::Cmd)
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
        let windows = conn.server.os == OsType::Windows;
        channel
            .request_pty(
                true,
                "xterm-256color",
                cols.max(20),
                rows.max(5),
                0,
                0,
                &interactive_pty_modes(windows),
            )
            .await
            .context("申请 PTY 失败")?;
        channel.request_shell(true).await.context("启动 shell 失败")?;
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
            let mut win_boot = if windows { Some(WinBoot::new()) } else { None };
            loop {
                let boot_deadline = win_boot.as_ref().and_then(|boot| boot.deadline());
                tokio::select! {
                    message = channel.wait() => {
                        let mut inject = None;
                        match &message {
                            Some(ChannelMsg::Data { data }) => {
                                let payload = TermDataPayload {
                                    session_id: task_session_id.clone(),
                                    data: base64::engine::general_purpose::STANDARD.encode(data),
                                };
                                let _ = app.emit("term-data", payload);
                                if let Some(boot) = win_boot.as_mut() {
                                    inject = boot.on_output(data.as_ref());
                                }
                            }
                            Some(ChannelMsg::ExtendedData { data, .. }) => {
                                let payload = TermDataPayload {
                                    session_id: task_session_id.clone(),
                                    data: base64::engine::general_purpose::STANDARD.encode(data),
                                };
                                let _ = app.emit("term-data", payload);
                                if let Some(boot) = win_boot.as_mut() {
                                    inject = boot.on_output(data.as_ref());
                                }
                            }
                            Some(ChannelMsg::ExitStatus { .. }) | Some(ChannelMsg::Close) | None => break,
                            _ => {}
                        }
                        if let Some(bytes) = inject {
                            if channel.data(bytes).await.is_err() { break; }
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
                    _ = async {
                        if let Some(deadline) = boot_deadline {
                            tokio::time::sleep(deadline.saturating_duration_since(Instant::now())).await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => {
                        if let Some(boot) = win_boot.as_mut() {
                            if let Some(bytes) = boot.on_timeout() {
                                if channel.data(bytes).await.is_err() { break; }
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

#[cfg(test)]
mod tests {
    use super::{detect_windows_prompt, WinPrompt};

    #[test]
    fn detect_cmd_prompt() {
        assert_eq!(
            detect_windows_prompt(b"Microsoft Windows\r\nC:\\Users\\hyin>"),
            Some(WinPrompt::Cmd)
        );
        assert_eq!(
            detect_windows_prompt(b"yinheng@10_1_8_16@10_1_8_16 C:\\Users\\yinheng>"),
            Some(WinPrompt::Cmd)
        );
    }

    #[test]
    fn detect_powershell_prompt() {
        assert_eq!(
            detect_windows_prompt(b"PS C:\\Users\\hyin>"),
            Some(WinPrompt::PowerShell)
        );
        assert_eq!(detect_windows_prompt(b"PS>"), Some(WinPrompt::PowerShell));
    }

    #[test]
    fn ignore_echoed_inject() {
        assert_eq!(
            detect_windows_prompt(b"C:\\Users\\hyin>powershell -NoLogo -NoProfile -NoExit"),
            None
        );
        assert_eq!(
            detect_windows_prompt(b"PS C:\\Users\\hyin>Remove-Module PSReadLine -ErrorAction SilentlyContinue; cls"),
            None
        );
    }
}
