//! SSH 连接、跳板、命令执行与 SFTP 辅助。主机密钥采用 TOFU（首次信任）。

use crate::config::{AppConfig, AuthConfig, OsType, ServerConfig};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use russh::client::{self, Handle};
use russh::keys::{key, load_secret_key};
use russh::ChannelMsg;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 主机指纹存储（TOFU：首次连接记录指纹，之后指纹变化则拒绝，防中间人）
fn load_known_hosts() -> HashMap<String, String> {
    let path = crate::config::known_hosts_file_path();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn save_known_hosts(map: &HashMap<String, String>) {
    let dir = crate::config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(content) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(crate::config::known_hosts_file_path(), content);
    }
}

/// russh 客户端事件处理器：负责主机密钥校验
pub struct SshHandler {
    /// host:port 形式，作为 known_hosts 的键
    host_key_id: String,
}

#[async_trait]
impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = server_public_key.fingerprint();
        let mut known = load_known_hosts();
        match known.get(&self.host_key_id) {
            None => {
                // 首次连接：信任并记录
                known.insert(self.host_key_id.clone(), fingerprint);
                save_known_hosts(&known);
                Ok(true)
            }
            Some(saved) if *saved == fingerprint => Ok(true),
            Some(saved) => Err(anyhow!(
                "主机 {} 的密钥指纹发生变化！可能存在中间人攻击。\n已保存: {}\n当前: {}\n如确认服务器已重装，请删除 known_hosts.json 中对应记录",
                self.host_key_id, saved, fingerprint
            )),
        }
    }
}

/// 一条 SSH 连接；持有跳板链上所有会话句柄以保活
pub struct SshConnection {
    pub handle: Handle<SshHandler>,
    pub server: ServerConfig,
    /// 跳板机会话必须保持存活，否则链路中断
    _parents: Vec<Handle<SshHandler>>,
    /// 直连上一跳跳板机配置（用于先把文件传到跳板再内网拷贝）
    pub jump_server: Option<ServerConfig>,
}

impl SshConnection {
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    pub fn nearest_jump_handle(&self) -> Option<&Handle<SshHandler>> {
        self._parents.last()
    }
}

/// 说明本机到该服务器怎么走（跳板机 ≠ 隧道页的端口映射）
pub fn describe_route(config: &AppConfig, server: &ServerConfig) -> String {
    match &server.jump_server_id {
        Some(jump_id) => {
            let jump_name = config
                .find_server(jump_id)
                .map(|jump| jump.name.as_str())
                .unwrap_or(jump_id);
            format!(
                "经跳板机 {} → {} ({}:{})",
                jump_name, server.name, server.host, server.port
            )
        }
        None => format!("直连 {} ({}:{})", server.name, server.host, server.port),
    }
}

/// 已建立连接时的路径说明
pub fn describe_connection(conn: &SshConnection) -> String {
    match &conn.jump_server {
        Some(jump) => format!(
            "经跳板机 {} ({}) → {} ({}:{})",
            jump.name, jump.host, conn.server.name, conn.server.host, conn.server.port
        ),
        None => format!(
            "直连 {} ({}:{})",
            conn.server.name, conn.server.host, conn.server.port
        ),
    }
}

fn client_config() -> Arc<client::Config> {
    Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        inactivity_timeout: None,
        // 加大窗口与单包，利于 RDP / 大流量转发
        window_size: 16 * 1024 * 1024,
        maximum_packet_size: 65535,
        ..Default::default()
    })
}

/// 直连 TCP 并关闭 Nagle，降低交互延迟（RDP / 隧道转发关键）
async fn connect_tcp_nodelay(host: &str, port: u16) -> Result<tokio::net::TcpStream> {
    let stream = tokio::net::TcpStream::connect((host, port))
        .await
        .with_context(|| format!("TCP 连接 {}:{} 失败", host, port))?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

async fn authenticate(handle: &mut Handle<SshHandler>, server: &ServerConfig) -> Result<()> {
    let success = match &server.auth {
        AuthConfig::Password { password } => {
            handle
                .authenticate_password(&server.username, password)
                .await?
        }
        AuthConfig::Key {
            key_path,
            passphrase,
        } => {
            let private_key = load_secret_key(key_path, passphrase.as_deref())
                .with_context(|| format!("加载私钥失败: {}", key_path))?;
            handle
                .authenticate_publickey(&server.username, Arc::new(private_key))
                .await?
        }
    };
    if !success {
        bail!("服务器 {} 认证失败，请检查用户名/密码/密钥", server.name);
    }
    Ok(())
}

/// 将底层连接/握手错误转成更易懂的中文说明
pub fn explain_connect_error(error: &anyhow::Error, server: &ServerConfig) -> String {
    let raw = format!("{:#}", error);
    let lower = raw.to_ascii_lowercase();
    let address = format!("{}:{}", server.host, server.port);
    let is_windows = matches!(server.os, OsType::Windows);
    let rdp_hint = if server.port == 3389 {
        " 端口 3389 通常是 RDP，本工具测试连接走 SSH 协议；请改为 OpenSSH 端口（默认 22），并确认已安装/启用 OpenSSH Server。"
    } else if is_windows {
        " Windows 请确认已开启 OpenSSH Server，端口一般为 22（不是 RDP 的 3389）。"
    } else {
        ""
    };

    if lower.contains("connection refused")
        || lower.contains("actively refused")
        || lower.contains("拒绝")
        || (lower.contains("wsaconnect") && lower.contains("10061"))
    {
        return format!(
            "连接被拒绝（{}）。目标未在该端口监听 SSH，或防火墙拦截。{}",
            address, rdp_hint
        );
    }
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("time out")
        || lower.contains("10060")
    {
        return format!(
            "连接超时（{}）。请检查主机地址、网络与防火墙。{}",
            address, rdp_hint
        );
    }
    if lower.contains("handshake")
        || lower.contains("protocol")
        || (lower.contains("ssh") && (lower.contains("banner") || lower.contains("version")))
        || lower.contains("reset by peer")
        || lower.contains("forcibly closed")
        || lower.contains("10054")
    {
        return format!(
            "SSH 协议握手失败（{}）。端口可达但对方不是 SSH 服务（例如填了 RDP 3389），或 OpenSSH 未正常工作。{}",
            address, rdp_hint
        );
    }
    if lower.contains("no route")
        || lower.contains("network unreachable")
        || lower.contains("11001")
    {
        return format!("无法到达主机（{}）。请检查地址与网络。", address);
    }
    format!("连接服务器 {}（{}）失败：{}{}", server.name, address, raw, rdp_hint)
}

/// 建立到目标服务器的连接；jump_server_id 存在时递归先连跳板机，
/// 再在跳板机上开 direct-tcpip 通道抵达目标
pub async fn connect(config: &AppConfig, server_id: &str) -> Result<SshConnection> {
    connect_inner(config, server_id, 0).await
}

async fn connect_inner(config: &AppConfig, server_id: &str, depth: u8) -> Result<SshConnection> {
    if depth > 4 {
        bail!("跳板机链过深或存在循环引用");
    }
    let server = config.find_server(server_id)?.clone();
    let handler = SshHandler {
        host_key_id: format!("{}:{}", server.host, server.port),
    };

    let (mut handle, parents, jump_server) = match &server.jump_server_id {
        None => {
            let stream = match connect_tcp_nodelay(&server.host, server.port).await {
                Ok(stream) => stream,
                Err(error) => {
                    bail!("{}", explain_connect_error(&error, &server));
                }
            };
            let handle = match client::connect_stream(client_config(), stream, handler).await {
                Ok(value) => value,
                Err(error) => {
                    bail!("{}", explain_connect_error(&anyhow!("{}", error), &server));
                }
            };
            (handle, Vec::new(), None)
        }
        Some(jump_id) => {
            let jump_conn = Box::pin(connect_inner(config, jump_id, depth + 1)).await?;
            let jump_server = jump_conn.server.clone();
            let channel = jump_conn
                .handle
                .channel_open_direct_tcpip(&server.host, server.port as u32, "127.0.0.1", 0)
                .await
                .map_err(|error| {
                    anyhow!(explain_connect_error(
                        &anyhow!(
                            "经跳板机打开到 {}:{} 的通道失败: {}",
                            server.host,
                            server.port,
                            error
                        ),
                        &server
                    ))
                })?;
            let handle = client::connect_stream(client_config(), channel.into_stream(), handler)
                .await
                .map_err(|error| {
                    anyhow!(explain_connect_error(
                        &anyhow!("经跳板机与 {} 握手失败: {}", server.name, error),
                        &server
                    ))
                })?;
            // 展开跳板链，全部保活
            let mut parents = jump_conn._parents;
            parents.push(jump_conn.handle);
            (handle, parents, Some(jump_server))
        }
    };

    authenticate(&mut handle, &server).await?;

    Ok(SshConnection {
        handle,
        server,
        _parents: parents,
        jump_server,
    })
}

/// 远程命令执行结果
pub struct ExecOutput {
    pub exit_code: u32,
    pub stdout: String,
    pub stderr: String,
}

impl ExecOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
    pub fn combined(&self) -> String {
        let mut text = strip_clixml(&self.stdout);
        let stderr = strip_clixml(&self.stderr);
        if !stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&stderr);
        }
        text
    }
}

/// PowerShell 经 SSH 非交互执行时，会把进度记录打成 CLIXML，需从输出里去掉
pub fn is_clixml_noise(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("#< CLIXML")
        || trimmed.contains("http://schemas.microsoft.com/powershell/2004")
}

/// 去掉 PowerShell CLIXML 进度噪声，保留真实错误文本
pub fn strip_clixml(text: &str) -> String {
    text.lines()
        .filter(|line| !is_clixml_noise(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// 字节转字符串：优先 UTF-8，失败则按 GBK 解码（中文 Windows cmd 默认编码）
fn decode_output(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let (decoded, _, _) = encoding_rs::GBK.decode(bytes);
            decoded.into_owned()
        }
    }
}

/// 执行远程命令并等待结束，可选逐行回调实时输出
pub async fn exec(
    conn: &SshConnection,
    command: &str,
    on_line: Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<ExecOutput> {
    exec_on(&conn.handle, command, on_line).await
}

/// 在指定 SSH 句柄上执行命令（可用于跳板机本身）
pub async fn exec_on(
    handle: &Handle<SshHandler>,
    command: &str,
    mut on_line: Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<ExecOutput> {
    let mut channel = handle.channel_open_session().await?;
    channel.exec(true, command).await?;

    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let mut exit_code: u32 = 0;
    let mut line_buffer: Vec<u8> = Vec::new();

    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { ref data } => {
                stdout.extend_from_slice(data);
                if let Some(callback) = on_line.as_mut() {
                    line_buffer.extend_from_slice(data);
                    while let Some(pos) = line_buffer.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<u8> = line_buffer.drain(..=pos).collect();
                        let text = decode_output(&line);
                        callback(text.trim_end_matches(['\r', '\n']));
                    }
                }
            }
            ChannelMsg::ExtendedData { ref data, ext } => {
                if ext == 1 {
                    stderr.extend_from_slice(data);
                }
            }
            ChannelMsg::ExitStatus { exit_status } => {
                exit_code = exit_status;
            }
            _ => {}
        }
    }
    // 输出剩余不带换行的内容
    if let Some(callback) = on_line.as_mut() {
        if !line_buffer.is_empty() {
            let text = decode_output(&line_buffer);
            callback(text.trim_end_matches(['\r', '\n']));
        }
    }

    Ok(ExecOutput {
        exit_code,
        stdout: decode_output(&stdout),
        stderr: decode_output(&stderr),
    })
}

/// 将 PowerShell 脚本包装成 EncodedCommand 形式执行，彻底避免 cmd/PS 双层引号转义问题
pub fn powershell_command(script: &str) -> String {
    // 关闭进度流，避免 SSH 捕获到 #< CLIXML 噪声
    let wrapped = format!("$ProgressPreference = 'SilentlyContinue'\n{}", script);
    let utf16_bytes: Vec<u8> = wrapped
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16_bytes);
    format!(
        "powershell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {}",
        encoded
    )
}

/// 单引号转义后包装为 Linux shell 命令
pub fn shell_command(script: &str) -> String {
    let escaped = script.replace('\'', "'\\''");
    format!("sh -lc '{}'", escaped)
}

/// 从探测命令输出解析系统类型
pub fn parse_detected_os(output: &str) -> Option<String> {
    let lower = output.to_lowercase();
    if lower.contains("windows") { return Some("windows".into()); }
    if lower.contains("ubuntu") { return Some("ubuntu".into()); }
    if lower.contains("centos")
        || lower.contains("rhel")
        || lower.contains("rocky")
        || lower.contains("almalinux")
    {
        return Some("centos".into());
    }
    if lower.contains("linux") { return Some("linux".into()); }
    None
}

/// 连接成功后执行探测命令，返回 (解析出的系统类型, 原始输出)
pub async fn probe_os(conn: &SshConnection) -> Result<(Option<String>, String)> {
    // Windows 不用 PowerShell/WMI：Get-CimInstance 首次加载模块经常要数秒
    let probe_command = match conn.server.os {
        OsType::Linux => shell_command(
            "uname -srm; [ -f /etc/os-release ] && grep -iE '^(ID|ID_LIKE|NAME)=' /etc/os-release",
        ),
        OsType::Windows => "cmd /c ver".to_string(),
    };
    let output = exec(conn, &probe_command, None).await?;
    let combined = output.combined().trim().to_string();
    let detected = parse_detected_os(&combined);
    Ok((detected, combined))
}

/// Windows OpenSSH 单包过大不稳定；经跳板时机密逐包确认，所以允许流水线并发写入
pub const SFTP_WRITE_CHUNK: usize = 32 * 1024;
const SFTP_WRITE_CHUNK_LINUX: usize = 128 * 1024;

/// 按目标系统选择 SFTP 写包大小
pub fn sftp_write_chunk(os: OsType) -> usize {
    match os {
        OsType::Windows => SFTP_WRITE_CHUNK,
        OsType::Linux => SFTP_WRITE_CHUNK_LINUX,
    }
}

fn sftp_concurrent_writes(os: OsType) -> usize {
    match os {
        OsType::Windows => 4,
        OsType::Linux => 4,
    }
}

/// 打开 SFTP 会话
pub async fn open_sftp(conn: &SshConnection) -> Result<russh_sftp::client::SftpSession> {
    open_sftp_handle(&conn.handle, conn.server.os).await
}

/// 在已有会话句柄上打开 SFTP（可指向跳板机）
pub async fn open_sftp_handle(
    handle: &Handle<SshHandler>,
    os: OsType,
) -> Result<russh_sftp::client::SftpSession> {
    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let chunk = sftp_write_chunk(os) as u32;
    let sftp_config = russh_sftp::client::Config {
        max_packet_len: chunk,
        max_concurrent_writes: sftp_concurrent_writes(os),
        request_timeout_secs: 120,
    };
    let sftp = russh_sftp::client::SftpSession::new_with_config(channel.into_stream(), sftp_config)
        .await
        .context("建立 SFTP 会话失败")?;
    sftp.set_timeout(120);
    Ok(sftp)
}

/// 远程路径统一使用正斜杠（Windows OpenSSH 的 SFTP 也接受 D:/xxx 形式）
pub fn to_sftp_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// 递归创建远端目录（已存在则忽略）
pub async fn sftp_mkdir_all(sftp: &russh_sftp::client::SftpSession, path: &str) -> Result<()> {
    let normalized = to_sftp_path(path);
    let segments: Vec<&str> = normalized.split('/').filter(|part| !part.is_empty()).collect();
    let mut current = String::new();
    let starts_with_root = normalized.starts_with('/');
    for (index, segment) in segments.iter().enumerate() {
        if index == 0 && !starts_with_root {
            // Windows 盘符段（如 D:）不需要创建
            current = segment.to_string();
            if segment.ends_with(':') {
                continue;
            }
        } else if current.is_empty() && starts_with_root {
            current = format!("/{}", segment);
        } else {
            current = format!("{}/{}", current, segment);
        }
        // 已存在则跳过
        if sftp.metadata(current.clone()).await.is_ok() {
            continue;
        }
        // 创建失败且确实不存在才报错（并发/竞态下容错）
        if let Err(error) = sftp.create_dir(current.clone()).await {
            if sftp.metadata(current.clone()).await.is_err() {
                return Err(anyhow!("创建远端目录 {} 失败: {}", current, error));
            }
        }
    }
    Ok(())
}

fn sh_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn windows_sftp_abs(path: &str) -> String {
    let normalized = to_sftp_path(path);
    if normalized.starts_with('/') {
        normalized
    } else {
        format!("/{}", normalized)
    }
}

/// 跳板机临时目录上的中转包，用完后应 cleanup
pub struct JumpStagedPayload {
    work_dir: String,
    payload_path: String,
}

async fn sftp_write_all(
    sftp: &russh_sftp::client::SftpSession,
    remote_path: &str,
    bytes: &[u8],
) -> Result<()> {
    let mut remote = sftp
        .create(remote_path.to_string())
        .await
        .with_context(|| format!("在跳板机创建文件失败: {}", remote_path))?;
    remote.write_all(bytes).await?;
    remote.flush().await?;
    remote.shutdown().await?;
    Ok(())
}

/// 把本地文件传到 Linux 跳板机临时目录，供后续内网分发
pub async fn stage_on_jump(
    conn: &SshConnection,
    local_path: &Path,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<JumpStagedPayload> {
    let jump_handle = conn
        .nearest_jump_handle()
        .context("没有跳板机会话，无法中转上传")?;
    let jump_server = conn
        .jump_server
        .as_ref()
        .context("没有跳板机配置，无法中转上传")?;
    if jump_server.os != OsType::Linux {
        bail!("跳板机 {} 不是 Linux，无法用内网拷贝中转", jump_server.name);
    }

    let stamp = uuid::Uuid::new_v4().to_string();
    let work_dir = format!("/tmp/shhd-deploy-{}", stamp);
    let payload_path = format!("{}/payload.bin", work_dir);
    let sftp = open_sftp_handle(jump_handle, OsType::Linux).await?;
    sftp_mkdir_all(&sftp, &work_dir).await?;

    let mut local_file = tokio::fs::File::open(local_path)
        .await
        .with_context(|| format!("打开本地文件失败: {}", local_path.display()))?;
    let total = local_file.metadata().await?.len();
    let mut remote = sftp
        .create(payload_path.clone())
        .await
        .with_context(|| format!("在跳板机创建中转文件失败: {}", payload_path))?;
    let chunk_size = sftp_write_chunk(OsType::Linux);
    let mut buffer = vec![0u8; chunk_size];
    let mut sent = 0u64;
    loop {
        let read = local_file.read(&mut buffer).await?;
        if read == 0 { break; }
        remote.write_all(&buffer[..read]).await?;
        sent += read as u64;
        on_progress(sent, total);
    }
    remote.flush().await?;
    remote.shutdown().await?;
    Ok(JumpStagedPayload {
        work_dir,
        payload_path,
    })
}

/// 跳板机已有的 zip 内网拷到一台 Windows
pub async fn copy_jump_payload_to_windows(
    jump_conn: &SshConnection,
    staging: &JumpStagedPayload,
    target: &SshConnection,
    windows_remote_path: &str,
) -> Result<()> {
    let jump_handle = jump_conn
        .nearest_jump_handle()
        .context("没有跳板机会话")?;
    let AuthConfig::Password { password } = &target.server.auth else {
        bail!("经跳板机中转需要目标 Windows 使用密码认证");
    };

    let dest_abs = windows_sftp_abs(windows_remote_path);
    let user_host = format!("{}@{}", target.server.username, target.server.host);
    let win_path = to_sftp_path(windows_remote_path);
    let win_sftp = open_sftp(target).await?;
    if let Some(slash) = win_path.rfind('/') {
        if slash > 0 { sftp_mkdir_all(&win_sftp, &win_path[..slash]).await?; }
    }
    drop(win_sftp);

    let tag = uuid::Uuid::new_v4().to_string();
    let askpass_path = format!("{}/askpass-{}.sh", staging.work_dir, tag);
    let batch_path = format!("{}/batch-{}.txt", staging.work_dir, tag);
    let run_path = format!("{}/run-{}.sh", staging.work_dir, tag);
    let askpass = format!(
        "#!/bin/sh\nprintf '%s\\n' {}\n",
        sh_single_quote(password)
    );
    let batch = format!("put {} {}\n", staging.payload_path, dest_abs);
    let run_script = format!(
        r#"#!/bin/sh
set -e
SRC={src}
ASKPASS={askpass}
BATCH={batch}
USERHOST={user_host}
REMOTE={remote}
if command -v scp >/dev/null 2>&1; then
  if scp -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null "$SRC" "$USERHOST:$REMOTE"; then
    exit 0
  fi
fi
if ! command -v sftp >/dev/null 2>&1; then
  echo "跳板机缺少 scp/sftp，无法内网拷贝到 Windows" >&2
  exit 1
fi
export DISPLAY="${{DISPLAY:-:0}}"
export SSH_ASKPASS="$ASKPASS"
export SSH_ASKPASS_REQUIRE=force
sftp -oPreferredAuthentications=password -oPubkeyAuthentication=no \
  -oStrictHostKeyChecking=no -oUserKnownHostsFile=/dev/null \
  -b "$BATCH" "$USERHOST" </dev/null
"#,
        src = sh_single_quote(&staging.payload_path),
        askpass = sh_single_quote(&askpass_path),
        batch = sh_single_quote(&batch_path),
        user_host = sh_single_quote(&user_host),
        remote = sh_single_quote(&dest_abs),
    );

    let sftp = open_sftp_handle(jump_handle, OsType::Linux).await?;
    sftp_write_all(&sftp, &askpass_path, askpass.as_bytes()).await?;
    sftp_write_all(&sftp, &batch_path, batch.as_bytes()).await?;
    sftp_write_all(&sftp, &run_path, run_script.as_bytes()).await?;
    drop(sftp);

    let copy_cmd = format!(
        "chmod 700 {} {} && sh {}",
        sh_single_quote(&askpass_path),
        sh_single_quote(&run_path),
        sh_single_quote(&run_path)
    );
    let output = exec_on(jump_handle, &copy_cmd, None).await?;
    if !output.success() {
        bail!(
            "跳板机内网拷贝到 {} 失败(退出码 {}): {}",
            target.server.name,
            output.exit_code,
            output.combined().chars().take(1500).collect::<String>()
        );
    }
    Ok(())
}

/// 删除跳板机上的中转临时目录（失败忽略）
pub async fn cleanup_jump_payload(conn: &SshConnection, staging: &JumpStagedPayload) {
    if let Some(jump_handle) = conn.nearest_jump_handle() {
        let _ = exec_on(
            jump_handle,
            &format!("rm -rf {}", sh_single_quote(&staging.work_dir)),
            None,
        )
        .await;
    }
}

/// 先把文件 SFTP 到 Linux 跳板机（公网走 Linux 大包），再由跳板机内网 scp/sftp 到 Windows
pub async fn upload_through_jump(
    conn: &SshConnection,
    local_path: &Path,
    windows_remote_path: &str,
    on_progress: impl FnMut(u64, u64),
) -> Result<()> {
    let staging = stage_on_jump(conn, local_path, on_progress).await?;
    let result = copy_jump_payload_to_windows(conn, &staging, conn, windows_remote_path).await;
    cleanup_jump_payload(conn, &staging).await;
    result
}
