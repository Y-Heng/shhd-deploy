use crate::config::config_dir;
use anyhow::{Context, Result};
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static LOG_ENABLED: AtomicBool = AtomicBool::new(false);
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 诊断日志目录（%APPDATA%/shhd-deploy/logs/）
pub fn log_dir() -> PathBuf {
    LOG_DIR
        .get_or_init(|| config_dir().join("logs"))
        .clone()
}

/// 当日日志文件路径
pub fn current_log_path() -> PathBuf {
    let date = Local::now().format("%Y%m%d");
    log_dir().join(format!("app-{}.log", date))
}

/// 根据配置初始化日志开关（关闭时几乎零开销）
pub fn init(enabled: bool) {
    LOG_ENABLED.store(enabled, Ordering::Relaxed);
}

/// 运行时切换日志开关
pub fn set_enabled(enabled: bool) {
    LOG_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    LOG_ENABLED.load(Ordering::Relaxed)
}

/// 追加一行诊断日志（仅 enabled 时落盘）
pub fn append_log(line: &str) {
    if !LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    let entry = format!("[{}] {}\n", timestamp, line);
    let _ = append_log_inner(&entry);
}

fn append_log_inner(entry: &str) -> Result<()> {
    let dir = log_dir();
    fs::create_dir_all(&dir)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(current_log_path())
        .with_context(|| format!("打开日志文件失败: {}", current_log_path().display()))?;
    file.write_all(entry.as_bytes())?;
    Ok(())
}

/// 读取最近若干行日志
pub fn read_recent_logs(max_lines: usize) -> Result<String> {
    let path = current_log_path();
    if !path.exists() {
        return Ok(String::new());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("读取日志失败: {}", path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= max_lines {
        return Ok(content);
    }
    Ok(lines[lines.len() - max_lines..].join("\n"))
}

/// 用资源管理器打开日志目录
pub fn open_log_dir() -> Result<()> {
    let dir = log_dir();
    fs::create_dir_all(&dir)?;
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&dir)
            .spawn()
            .with_context(|| format!("打开日志目录失败: {}", dir.display()))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        return Err(anyhow::anyhow!(
            "当前系统请手动打开目录: {}",
            dir.display()
        ));
    }
    Ok(())
}
