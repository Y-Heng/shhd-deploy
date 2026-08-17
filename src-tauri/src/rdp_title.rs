//! 把 mstsc 标题里的「远程桌面连接」换成 SSH 服务器名称，方便多开时区分。

use std::time::{Duration, Instant};

type WindowHandle = *mut std::ffi::c_void;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(
        callback: unsafe extern "system" fn(WindowHandle, isize) -> i32,
        lparam: isize,
    ) -> i32;
    fn GetWindowTextW(hwnd: WindowHandle, buffer: *mut u16, max_count: i32) -> i32;
    fn SetWindowTextW(hwnd: WindowHandle, text: *const u16) -> i32;
    fn IsWindowVisible(hwnd: WindowHandle) -> i32;
}

const GENERIC_TITLE_SUFFIXES: &[&str] = &[
    "远程桌面连接",
    "遠端桌面連線",
    "Remote Desktop Connection",
];

struct TitlePatch {
    address: String,
    desired_title: String,
    found: bool,
}

/// 后台轮询 mstsc 窗口，把通用后缀替换为服务器名称（连接过程中标题可能被系统改回）
pub fn watch_and_set_title(address: String, server_name: String) {
    let server_name = server_name.trim().to_string();
    if server_name.is_empty() || address.is_empty() { return; }
    std::thread::spawn(move || {
        let desired_title = format!("{} - {}", address, server_name);
        let find_deadline = Instant::now() + Duration::from_secs(90);
        let mut seen = false;
        let mut missing_since: Option<Instant> = None;
        loop {
            let found = patch_matching_windows(&address, &desired_title);
            if found {
                seen = true;
                missing_since = None;
            } else if seen {
                let started_missing = missing_since.get_or_insert_with(Instant::now);
                if started_missing.elapsed() > Duration::from_secs(3) { break; }
            } else if Instant::now() > find_deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    });
}

fn patch_matching_windows(address: &str, desired_title: &str) -> bool {
    let mut state = TitlePatch {
        address: address.to_string(),
        desired_title: desired_title.to_string(),
        found: false,
    };
    unsafe {
        EnumWindows(enum_windows_callback, &mut state as *mut TitlePatch as isize);
    }
    state.found
}

unsafe extern "system" fn enum_windows_callback(hwnd: WindowHandle, lparam: isize) -> i32 {
    let state = unsafe { &mut *(lparam as *mut TitlePatch) };
    if unsafe { IsWindowVisible(hwnd) } == 0 { return 1; }
    let title = window_title(hwnd);
    if title == state.desired_title {
        state.found = true;
        return 1;
    }
    if !should_replace_title(&title, &state.address) { return 1; }
    let wide_title: Vec<u16> = state
        .desired_title
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe { SetWindowTextW(hwnd, wide_title.as_ptr()); }
    state.found = true;
    1
}

fn should_replace_title(title: &str, address: &str) -> bool {
    let prefix = format!("{} - ", address);
    if !title.starts_with(&prefix) { return false; }
    GENERIC_TITLE_SUFFIXES.iter().any(|suffix| title.ends_with(suffix))
}

fn window_title(hwnd: WindowHandle) -> String {
    let mut buffer = [0u16; 512];
    let length = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 0 { return String::new(); }
    String::from_utf16_lossy(&buffer[..length as usize])
}
