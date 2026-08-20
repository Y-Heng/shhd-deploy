//! 本机文件系统浏览：磁盘列表与目录列举，供 SFTP 左栏使用。

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

/// 本地目录条目（字段与 SftpEntry 对齐，便于前端共用展示逻辑）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalDirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u32,
    /// 点文件或 Windows 隐藏属性
    pub hidden: bool,
}

/// 获取用户主目录（Windows 优先 USERPROFILE）
pub fn get_home_dir() -> Result<String> {
    if let Some(home) = dirs::home_dir() {
        return Ok(home.to_string_lossy().to_string());
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() {
            return Ok(profile);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Ok(home);
        }
    }
    bail!("无法获取用户主目录");
}

/// 列出本机磁盘（Windows 用 GetLogicalDrives，避免空光驱卡住）
#[cfg(windows)]
mod win_drives {
    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetLogicalDrives() -> u32;
    }
}

pub fn list_local_drives() -> Result<Vec<LocalDirEntry>> {
    #[cfg(windows)]
    {
        let mask = unsafe { win_drives::GetLogicalDrives() };
        let mut result = Vec::new();
        for index in 0..26u32 {
            if mask & (1 << index) == 0 {
                continue;
            }
            let letter = (b'A' + index as u8) as char;
            let root = format!("{}:\\", letter);
            result.push(LocalDirEntry {
                name: format!("{}:", letter),
                path: root,
                is_dir: true,
                size: 0,
                mtime: 0,
                hidden: false,
            });
        }
        if result.is_empty() {
            bail!("没有找到可用磁盘");
        }
        return Ok(result);
    }
    #[cfg(not(windows))]
    {
        Ok(vec![LocalDirEntry {
            name: "/".into(),
            path: "/".into(),
            is_dir: true,
            size: 0,
            mtime: 0,
            hidden: false,
        }])
    }
}

/// 列出本地目录；空路径时使用主目录
pub fn list_local_dir(path: &str) -> Result<Vec<LocalDirEntry>> {
    let dir_path = if path.trim().is_empty() {
        PathBuf::from(get_home_dir()?)
    } else {
        PathBuf::from(path)
    };

    if !dir_path.exists() {
        bail!("路径不存在: {}", dir_path.display());
    }
    if !dir_path.is_dir() {
        bail!("路径不是目录: {}", dir_path.display());
    }

    let read_dir = fs::read_dir(&dir_path)
        .with_context(|| format!("读取本地目录失败: {}", dir_path.display()))?;

    let mut result = Vec::new();
    for entry_result in read_dir {
        let entry = match entry_result {
            Ok(value) => value,
            Err(_) => continue,
        };
        let metadata = match entry.metadata() {
            Ok(value) => value,
            Err(_) => continue,
        };
        let file_name = entry.file_name().to_string_lossy().to_string();
        if file_name == "." || file_name == ".." {
            continue;
        }
        let absolute = entry.path();
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as u32)
            .unwrap_or(0);
        result.push(LocalDirEntry {
            name: file_name.clone(),
            path: absolute.to_string_lossy().to_string(),
            is_dir: metadata.is_dir(),
            size: if metadata.is_file() {
                metadata.len()
            } else {
                0
            },
            mtime,
            hidden: is_local_hidden(&file_name, &metadata),
        });
    }

    result.sort_by(|left, right| match (left.is_dir, right.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
    });
    Ok(result)
}

/// 点文件视为隐藏；Windows 再叠加系统隐藏属性
fn is_local_hidden(file_name: &str, metadata: &fs::Metadata) -> bool {
    if file_name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        if metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0 {
            return true;
        }
    }
    let _ = metadata;
    false
}
