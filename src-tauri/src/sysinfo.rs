//! 服务器系统状态采集模块（CPU、内存、Swap、进程、网络流量、磁盘挂载等）
//! 支持 Linux（通过 /proc 和标准工具）与 Windows（通过 PowerShell）

use crate::config::{AppConfig, OsType};
use crate::ssh::{self, SshConnection};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub memory_display: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiskMount {
    pub filesystem: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub use_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInterface {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_speed_bytes: f64,
    pub tx_speed_bytes: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SystemStats {
    pub server_id: String,
    pub uptime_seconds: u64,
    pub uptime_display: String,
    pub load_1: f32,
    pub load_5: f32,
    pub load_15: f32,
    pub cpu_percent: f32,
    pub memory_total: u64,
    pub memory_used: u64,
    pub memory_free: u64,
    pub memory_percent: f32,
    pub swap_total: u64,
    pub swap_used: u64,
    pub swap_percent: f32,
    pub rx_speed_bytes: f64,
    pub tx_speed_bytes: f64,
    pub top_processes: Vec<ProcessInfo>,
    pub disks: Vec<DiskMount>,
    pub networks: Vec<NetworkInterface>,
    pub timestamp_ms: u64,
}

/// 上一次采集的差值基准（用于计算 CPU 利用率与网络速率）
#[derive(Clone, Default)]
struct SampleSnapshot {
    timestamp: Option<Instant>,
    cpu_total: u64,
    cpu_idle: u64,
    net_bytes: HashMap<String, (u64, u64)>, // iface -> (rx_bytes, tx_bytes)
}

struct SysinfoSession {
    conn: SshConnection,
    last_sample: SampleSnapshot,
}

#[derive(Default)]
pub struct SysinfoManager {
    sessions: Arc<Mutex<HashMap<String, SysinfoSession>>>,
    active_monitors: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

impl SysinfoManager {
    /// 确保存在已连接的 SSH 会话，若断开则自动重连
    async fn get_or_connect(
        &self,
        config: &AppConfig,
        server_id: &str,
    ) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        let needs_connect = match sessions.get(server_id) {
            Some(s) => s.conn.is_closed(),
            None => true,
        };
        if needs_connect {
            sessions.remove(server_id);
            let conn = ssh::connect(config, server_id).await?;
            sessions.insert(
                server_id.to_string(),
                SysinfoSession {
                    conn,
                    last_sample: SampleSnapshot::default(),
                },
            );
        }
        Ok(())
    }

    /// 采集一次系统信息
    pub async fn fetch_stats(
        &self,
        config: &AppConfig,
        server_id: &str,
    ) -> Result<SystemStats> {
        self.get_or_connect(config, server_id).await?;
        let mut sessions = self.sessions.lock().await;
        let session = match sessions.get_mut(server_id) {
            Some(s) => s,
            None => bail!("服务器会话未初始化"),
        };

        let now = Instant::now();
        let elapsed_secs = session
            .last_sample
            .timestamp
            .map(|t| (now - t).as_secs_f64())
            .unwrap_or(1.0)
            .max(0.001);

        match session.conn.server.os {
            OsType::Linux => {
                let stats = collect_linux_stats(
                    &session.conn,
                    &mut session.last_sample,
                    elapsed_secs,
                    server_id,
                )
                .await?;
                session.last_sample.timestamp = Some(now);
                Ok(stats)
            }
            OsType::Windows => {
                let stats = collect_windows_stats(
                    &session.conn,
                    &mut session.last_sample,
                    elapsed_secs,
                    server_id,
                )
                .await?;
                session.last_sample.timestamp = Some(now);
                Ok(stats)
            }
        }
    }

    /// 开启对某台服务器的定时监控，定期广播 `sysinfo-stats` 事件
    pub async fn start_monitoring(
        &self,
        app: AppHandle,
        config: AppConfig,
        server_id: String,
        interval_secs: u64,
    ) {
        let mut monitors = self.active_monitors.write().await;
        if monitors.contains_key(&server_id) {
            return;
        }

        let sessions = self.sessions.clone();
        let sid = server_id.clone();
        let handle = tokio::spawn(async move {
            let target_os = config.find_server(&sid).map(|s| s.os).unwrap_or(OsType::Linux);
            // Windows 执行 PowerShell 相对重，采样周期拉长至 4 秒；Linux 保持 2 秒
            let default_sec = if target_os == OsType::Windows { 4 } else { 2 };
            let poll_sec = interval_secs.max(default_sec);
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(poll_sec));
            loop {
                interval.tick().await;

                // 检查会话并采集
                let res = async {
                    let mut sess_guard = sessions.lock().await;
                    let needs_connect = match sess_guard.get(&sid) {
                        Some(s) => s.conn.is_closed(),
                        None => true,
                    };
                    if needs_connect {
                        sess_guard.remove(&sid);
                        let conn = ssh::connect(&config, &sid).await?;
                        sess_guard.insert(
                            sid.clone(),
                            SysinfoSession {
                                conn,
                                last_sample: SampleSnapshot::default(),
                            },
                        );
                    }
                    let session = sess_guard.get_mut(&sid).context("会话丢失")?;
                    let now = Instant::now();
                    let elapsed_secs = session
                        .last_sample
                        .timestamp
                        .map(|t| (now - t).as_secs_f64())
                        .unwrap_or(1.0)
                        .max(0.001);

                    let stats = match session.conn.server.os {
                        OsType::Linux => {
                            collect_linux_stats(
                                &session.conn,
                                &mut session.last_sample,
                                elapsed_secs,
                                &sid,
                            )
                            .await?
                        }
                        OsType::Windows => {
                            collect_windows_stats(
                                &session.conn,
                                &mut session.last_sample,
                                elapsed_secs,
                                &sid,
                            )
                            .await?
                        }
                    };
                    session.last_sample.timestamp = Some(now);
                    Ok::<SystemStats, anyhow::Error>(stats)
                }
                .await;

                match res {
                    Ok(stats) => {
                        let _ = app.emit("sysinfo-stats", stats);
                    }
                    Err(e) => {
                        let _ = app.emit(
                            "sysinfo-error",
                            serde_json::json!({
                                "serverId": sid,
                                "error": format!("{:#}", e)
                            }),
                        );
                    }
                }
            }
        });

        monitors.insert(server_id, handle);
    }

    /// 停止对某台服务器的定时采集
    pub async fn stop_monitoring(&self, server_id: &str) {
        let mut monitors = self.active_monitors.write().await;
        if let Some(handle) = monitors.remove(server_id) {
            handle.abort();
        }
        let mut sessions = self.sessions.lock().await;
        sessions.remove(server_id);
    }
}

/// 格式化时间长度为 "85 天 12 小时" 或 "2 小时 30 分"
pub fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86400;
    let hours = (seconds % 86400) / 3600;
    let minutes = (seconds % 3600) / 60;
    if days > 0 {
        format!("{} 天 {} 小时", days, hours)
    } else if hours > 0 {
        format!("{} 小时 {} 分", hours, minutes)
    } else {
        format!("{} 分钟", minutes.max(1))
    }
}

/// 格式化字节大小为可读字符串（如 7.6G, 556.4M）
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const TB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= TB {
        format!("{:.1}T", b / TB)
    } else if b >= GB {
        format!("{:.1}G", b / GB)
    } else if b >= MB {
        format!("{:.1}M", b / MB)
    } else if b >= KB {
        format!("{:.1}K", b / KB)
    } else {
        format!("{}B", bytes)
    }
}

// ==================== Linux 采集实现 ====================

/// Linux 系统执行多合一采集脚本，解析 stdout
async fn collect_linux_stats(
    conn: &SshConnection,
    last_sample: &mut SampleSnapshot,
    elapsed_secs: f64,
    server_id: &str,
) -> Result<SystemStats> {
    // 使用单一命令组合读取 /proc/uptime, /proc/loadavg, /proc/stat, /proc/meminfo, /proc/net/dev, df, ps
    // 各段之间用统一标记符分隔
    let script = r#"
echo "===UPTIME==="
cat /proc/uptime 2>/dev/null || uptime
echo "===LOADAVG==="
cat /proc/loadavg 2>/dev/null
echo "===STAT==="
cat /proc/stat 2>/dev/null | grep '^cpu '
echo "===MEMINFO==="
cat /proc/meminfo 2>/dev/null | grep -E '^(MemTotal|MemFree|MemAvailable|Buffers|Cached|SwapTotal|SwapFree):'
echo "===NET==="
cat /proc/net/dev 2>/dev/null | tail -n +3
echo "===DF==="
df -B1 -P 2>/dev/null | tail -n +2
echo "===PS==="
ps -eo pid,%cpu,rss,comm --sort=-%cpu 2>/dev/null | head -n 26 | tail -n +2
echo "===END==="
"#;

    let command = ssh::shell_command(script.trim());
    let output = ssh::exec(conn, &command, None).await?;
    let raw = output.stdout;

    parse_linux_output(&raw, last_sample, elapsed_secs, server_id)
}

fn parse_linux_output(
    raw: &str,
    last_sample: &mut SampleSnapshot,
    elapsed_secs: f64,
    server_id: &str,
) -> Result<SystemStats> {
    let mut sections: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut current_section = "";

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("===") && trimmed.ends_with("===") {
            current_section = trimmed.trim_matches('=');
            continue;
        }
        if !current_section.is_empty() {
            sections.entry(current_section).or_default().push(line);
        }
    }

    // 1. Uptime
    let mut uptime_seconds = 0u64;
    if let Some(lines) = sections.get("UPTIME") {
        if let Some(first) = lines.first() {
            if let Some(val_str) = first.split_whitespace().next() {
                if let Ok(val) = val_str.parse::<f64>() {
                    uptime_seconds = val as u64;
                }
            }
        }
    }
    let uptime_display = format_uptime(uptime_seconds);

    // 2. Load average
    let mut load_1 = 0.0f32;
    let mut load_5 = 0.0f32;
    let mut load_15 = 0.0f32;
    if let Some(lines) = sections.get("LOADAVG") {
        if let Some(first) = lines.first() {
            let parts: Vec<&str> = first.split_whitespace().collect();
            if parts.len() >= 3 {
                load_1 = parts[0].parse().unwrap_or(0.0);
                load_5 = parts[1].parse().unwrap_or(0.0);
                load_15 = parts[2].parse().unwrap_or(0.0);
            }
        }
    }

    // 3. CPU percent
    let mut cpu_percent = 0.0f32;
    if let Some(lines) = sections.get("STAT") {
        if let Some(first) = lines.first() {
            // cpu  user nice system idle iowait irq softirq steal guest guest_nice
            let parts: Vec<u64> = first
                .split_whitespace()
                .skip(1)
                .filter_map(|s| s.parse().ok())
                .collect();
            if parts.len() >= 4 {
                let idle = parts[3] + parts.get(4).copied().unwrap_or(0);
                let total: u64 = parts.iter().sum();

                if last_sample.cpu_total > 0 && total > last_sample.cpu_total {
                    let total_delta = total - last_sample.cpu_total;
                    let idle_delta = idle.saturating_sub(last_sample.cpu_idle);
                    let busy_delta = total_delta.saturating_sub(idle_delta);
                    cpu_percent = ((busy_delta as f64 / total_delta as f64) * 100.0) as f32;
                }
                last_sample.cpu_total = total;
                last_sample.cpu_idle = idle;
            }
        }
    }

    // 4. Memory & Swap
    let mut mem_total = 0u64;
    let mut mem_free = 0u64;
    let mut mem_available = 0u64;
    let mut mem_buffers = 0u64;
    let mut mem_cached = 0u64;
    let mut swap_total = 0u64;
    let mut swap_free = 0u64;

    if let Some(lines) = sections.get("MEMINFO") {
        for line in lines {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() == 2 {
                let key = parts[0].trim();
                let val_kb = parts[1]
                    .trim()
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let val_bytes = val_kb * 1024;
                match key {
                    "MemTotal" => mem_total = val_bytes,
                    "MemFree" => mem_free = val_bytes,
                    "MemAvailable" => mem_available = val_bytes,
                    "Buffers" => mem_buffers = val_bytes,
                    "Cached" => mem_cached = val_bytes,
                    "SwapTotal" => swap_total = val_bytes,
                    "SwapFree" => swap_free = val_bytes,
                    _ => {}
                }
            }
        }
    }

    let memory_used = if mem_available > 0 {
        mem_total.saturating_sub(mem_available)
    } else {
        mem_total.saturating_sub(mem_free + mem_buffers + mem_cached)
    };
    let memory_percent = if mem_total > 0 {
        ((memory_used as f64 / mem_total as f64) * 100.0) as f32
    } else {
        0.0
    };

    let swap_used = swap_total.saturating_sub(swap_free);
    let swap_percent = if swap_total > 0 {
        ((swap_used as f64 / swap_total as f64) * 100.0) as f32
    } else {
        0.0
    };

    // 5. Network interfaces & speeds
    let mut networks = Vec::new();
    let mut total_rx_speed = 0.0f64;
    let mut total_tx_speed = 0.0f64;

    if let Some(lines) = sections.get("NET") {
        for line in lines {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() == 2 {
                let iface_name = parts[0].trim().to_string();
                if iface_name == "lo" {
                    continue;
                }
                let data_fields: Vec<u64> = parts[1]
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if data_fields.len() >= 9 {
                    let rx_bytes = data_fields[0];
                    let tx_bytes = data_fields[8];

                    let (rx_speed, tx_speed) = match last_sample.net_bytes.get(&iface_name) {
                        Some(&(prev_rx, prev_tx)) => {
                            let r_delta = rx_bytes.saturating_sub(prev_rx) as f64 / elapsed_secs;
                            let t_delta = tx_bytes.saturating_sub(prev_tx) as f64 / elapsed_secs;
                            (r_delta, t_delta)
                        }
                        None => (0.0, 0.0),
                    };

                    last_sample.net_bytes.insert(iface_name.clone(), (rx_bytes, tx_bytes));
                    total_rx_speed += rx_speed;
                    total_tx_speed += tx_speed;

                    networks.push(NetworkInterface {
                        name: iface_name,
                        rx_bytes,
                        tx_bytes,
                        rx_speed_bytes: rx_speed,
                        tx_speed_bytes: tx_speed,
                    });
                }
            }
        }
    }

    // 6. Disks
    let mut disks = Vec::new();
    if let Some(lines) = sections.get("DF") {
        for line in lines {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Filesystem 1024-blocks Used Available Capacity Mounted on
            if parts.len() >= 6 {
                let fs = parts[0];
                let total: u64 = parts[1].parse().unwrap_or(0);
                let used: u64 = parts[2].parse().unwrap_or(0);
                let free: u64 = parts[3].parse().unwrap_or(0);
                let mount = parts[5];

                if total > 0 && !mount.starts_with("/sys/firmware") && !mount.starts_with("/snap") {
                    let pct = ((used as f64 / total as f64) * 100.0) as f32;
                    disks.push(DiskMount {
                        filesystem: fs.to_string(),
                        mount_point: mount.to_string(),
                        total_bytes: total,
                        used_bytes: used,
                        free_bytes: free,
                        use_percent: pct,
                    });
                }
            }
        }
    }

    // 7. Top Processes
    let mut top_processes = Vec::new();
    if let Some(lines) = sections.get("PS") {
        for line in lines {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let pid: u32 = parts[0].parse().unwrap_or(0);
                let cpu_p: f32 = parts[1].parse().unwrap_or(0.0);
                let rss_kb: u64 = parts[2].parse().unwrap_or(0);
                let mem_bytes = rss_kb * 1024;
                let name = parts[3..].join(" ");
                top_processes.push(ProcessInfo {
                    pid,
                    name,
                    cpu_percent: cpu_p,
                    memory_bytes: mem_bytes,
                    memory_display: format_bytes(mem_bytes),
                });
            }
        }
    }

    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    Ok(SystemStats {
        server_id: server_id.to_string(),
        uptime_seconds,
        uptime_display,
        load_1,
        load_5,
        load_15,
        cpu_percent: cpu_percent.clamp(0.0, 100.0),
        memory_total: mem_total,
        memory_used,
        memory_free: mem_available.max(mem_free),
        memory_percent: memory_percent.clamp(0.0, 100.0),
        swap_total,
        swap_used,
        swap_percent: swap_percent.clamp(0.0, 100.0),
        rx_speed_bytes: total_rx_speed,
        tx_speed_bytes: total_tx_speed,
        top_processes,
        disks,
        networks,
        timestamp_ms,
    })
}

// ==================== Windows 采集实现 ====================

async fn collect_windows_stats(
    conn: &SshConnection,
    last_sample: &mut SampleSnapshot,
    elapsed_secs: f64,
    server_id: &str,
) -> Result<SystemStats> {
    // 使用简洁高效的 PowerShell 脚本采集基础指标
    // 使用合并查询一次性取出 OS、CPU、驱动器和进程，大幅减少 WMI 往返
    let ps_script = r#"
$ErrorActionPreference = 'SilentlyContinue'
$os = Get-CimInstance Win32_OperatingSystem
$uptime = [Math]::Floor(((Get-Date) - $os.LastBootUpTime).TotalSeconds)
$totalMem = [uint64]$os.TotalVisibleMemorySize * 1024
$freeMem = [uint64]$os.FreePhysicalMemory * 1024
$usedMem = $totalMem - $freeMem

$cpu = (Get-CimInstance Win32_Processor | Measure-Object -Property LoadPercentage -Average).Average
if (-not $cpu) { $cpu = 0 }

$page = Get-CimInstance Win32_PageFileUsage
$swapTotal = [uint64]($page | Measure-Object -Property AllocatedBaseSize -Sum).Sum * 1024 * 1024
$swapUsed = [uint64]($page | Measure-Object -Property CurrentUsage -Sum).Sum * 1024 * 1024

$drives = Get-CimInstance Win32_LogicalDisk -Filter "DriveType=3" | Select-Object DeviceID, Size, FreeSpace
$procs = Get-Process | Sort-Object -Descending WorkingSet64 | Select-Object -First 15 Id, ProcessName, WorkingSet64, CPU

[PSCustomObject]@{
    Uptime = $uptime
    Cpu = [math]::Round($cpu, 1)
    TotalMem = $totalMem
    UsedMem = $usedMem
    FreeMem = $freeMem
    SwapTotal = $swapTotal
    SwapUsed = $swapUsed
    Drives = @($drives | ForEach-Object {
        [PSCustomObject]@{
            Device = $_.DeviceID
            Total = [uint64]$_.Size
            Free = [uint64]$_.FreeSpace
            Used = [uint64]($_.Size - $_.FreeSpace)
        }
    })
    Processes = @($procs | ForEach-Object {
        [PSCustomObject]@{
            Pid = $_.Id
            Name = $_.ProcessName
            Mem = [uint64]$_.WorkingSet64
            Cpu = [math]::Round($_.CPU, 1)
        }
    })
} | ConvertTo-Json -Compress -Depth 4
"#;

    let command = ssh::powershell_command(ps_script.trim());
    let output = ssh::exec(conn, &command, None).await?;
    let raw = output.stdout;

    parse_windows_output(&raw, last_sample, elapsed_secs, server_id)
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WinDiskDto {
    device: Option<String>,
    total: Option<u64>,
    free: Option<u64>,
    used: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WinProcDto {
    pid: Option<u32>,
    name: Option<String>,
    mem: Option<u64>,
    cpu: Option<f32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WinStatsDto {
    uptime: Option<u64>,
    cpu: Option<f32>,
    total_mem: Option<u64>,
    used_mem: Option<u64>,
    free_mem: Option<u64>,
    swap_total: Option<u64>,
    swap_used: Option<u64>,
    drives: Option<Vec<WinDiskDto>>,
    processes: Option<Vec<WinProcDto>>,
}

fn parse_windows_output(
    raw: &str,
    _last_sample: &mut SampleSnapshot,
    _elapsed_secs: f64,
    server_id: &str,
) -> Result<SystemStats> {
    // 寻找 JSON 片段
    let json_start = raw.find('{').context("找不到有效的 JSON 输出")?;
    let json_end = raw.rfind('}').context("找不到有效的 JSON 输出")?;
    let json_str = &raw[json_start..=json_end];

    let dto: WinStatsDto = serde_json::from_str(json_str)
        .with_context(|| format!("解析 Windows 统计 JSON 失败: {}", json_str))?;

    let uptime_seconds = dto.uptime.unwrap_or(0);
    let uptime_display = format_uptime(uptime_seconds);

    let mem_total = dto.total_mem.unwrap_or(0);
    let mem_used = dto.used_mem.unwrap_or(0);
    let mem_percent = if mem_total > 0 {
        ((mem_used as f64 / mem_total as f64) * 100.0) as f32
    } else {
        0.0
    };

    let swap_total = dto.swap_total.unwrap_or(0);
    let swap_used = dto.swap_used.unwrap_or(0);
    let swap_percent = if swap_total > 0 {
        ((swap_used as f64 / swap_total as f64) * 100.0) as f32
    } else {
        0.0
    };

    let disks = dto
        .drives
        .unwrap_or_default()
        .into_iter()
        .map(|d| {
            let total = d.total.unwrap_or(0);
            let used = d.used.unwrap_or(0);
            let free = d.free.unwrap_or(0);
            let dev = d.device.unwrap_or_else(|| "C:".into());
            let pct = if total > 0 {
                ((used as f64 / total as f64) * 100.0) as f32
            } else {
                0.0
            };
            DiskMount {
                filesystem: dev.clone(),
                mount_point: dev,
                total_bytes: total,
                used_bytes: used,
                free_bytes: free,
                use_percent: pct,
            }
        })
        .collect();

    let top_processes = dto
        .processes
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let mem = p.mem.unwrap_or(0);
            ProcessInfo {
                pid: p.pid.unwrap_or(0),
                name: p.name.unwrap_or_default(),
                cpu_percent: p.cpu.unwrap_or(0.0),
                memory_bytes: mem,
                memory_display: format_bytes(mem),
            }
        })
        .collect();

    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    Ok(SystemStats {
        server_id: server_id.to_string(),
        uptime_seconds,
        uptime_display,
        load_1: 0.0,
        load_5: 0.0,
        load_15: 0.0,
        cpu_percent: dto.cpu.unwrap_or(0.0).clamp(0.0, 100.0),
        memory_total: mem_total,
        memory_used: mem_used,
        memory_free: dto.free_mem.unwrap_or(0),
        memory_percent: mem_percent.clamp(0.0, 100.0),
        swap_total,
        swap_used,
        swap_percent: swap_percent.clamp(0.0, 100.0),
        rx_speed_bytes: 0.0,
        tx_speed_bytes: 0.0,
        top_processes,
        disks,
        networks: vec![],
        timestamp_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_uptime() {
        assert_eq!(format_uptime(85 * 86400 + 3600), "85 天 1 小时");
        assert_eq!(format_uptime(7200 + 60), "2 小时 1 分");
        assert_eq!(format_uptime(45), "1 分钟");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(1024 * 1024 * 1024 * 7 + 1024 * 1024 * 600), "7.6G");
        assert_eq!(format_bytes(1024 * 1024 * 556), "556.0M");
    }

    #[test]
    fn test_parse_linux_output() {
        let sample = r#"
===UPTIME===
7344000.50 1234567.89
===LOADAVG===
12.03 11.30 11.03 5/1234 45678
===STAT===
cpu  2255 34 2290 22625563 6290 127 456 0 0 0
===MEMINFO===
MemTotal:       131287040 kB
MemFree:         33554432 kB
MemAvailable:    28311552 kB
Buffers:          1048576 kB
Cached:          16777216 kB
SwapTotal:       16777216 kB
SwapFree:         8388608 kB
===NET===
  eth0: 1000000000 1000 0 0 0 0 0 0 2000000000 2000 0 0 0 0 0 0
===DF===
/dev/sda1 106430464000 63102976000 43327488000 60% /
/dev/sdb1 209715200000 16777216000 192937984000 8% /home
===PS===
 1001 107.9 7969177 java
 1002 90.5 2936012 java
 1003 89.8 569753 MediaServer
===END===
"#;
        let mut snapshot = SampleSnapshot::default();
        let stats = parse_linux_output(sample, &mut snapshot, 1.0, "test_server").unwrap();

        assert_eq!(stats.uptime_seconds, 7344000);
        assert_eq!(stats.uptime_display, "85 天 0 小时");
        assert_eq!(stats.load_1, 12.03);
        assert_eq!(stats.load_5, 11.30);
        assert_eq!(stats.load_15, 11.03);
        assert_eq!(stats.swap_percent, 50.0);
        assert_eq!(stats.disks.len(), 2);
        assert_eq!(stats.top_processes.len(), 3);
        assert_eq!(stats.top_processes[0].name, "java");
        assert_eq!(stats.top_processes[0].cpu_percent, 107.9);
    }
}
