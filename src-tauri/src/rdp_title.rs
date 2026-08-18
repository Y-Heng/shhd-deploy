//! 改写 mstsc 标题，并在对应窗口消失后视为会话结束。
//! 多开时后续 mstsc 往往立刻退出、把窗口交给已有进程，不能只盯 Child。

use std::process::Child;
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

/// 持续改标题，直到该地址的远程桌面窗口消失（或确认根本没开起来）
pub fn watch_session(address: &str, server_name: &str, child: &mut Child) {
    let desired_title = {
        let name = server_name.trim();
        if name.is_empty() { format!("{address} - 远程桌面连接") }
        else { format!("{address} - {name}") }
    };
    let started = Instant::now();
    let mut seen_window = false;
    let mut missing_since: Option<Instant> = None;
    let mut child_lived: Option<Duration> = None;

    loop {
        let found = patch_matching_windows(address, &desired_title);
        if found {
            seen_window = true;
            missing_since = None;
        } else if seen_window {
            let gone_at = missing_since.get_or_insert_with(Instant::now);
            if gone_at.elapsed() > Duration::from_millis(800) { return; }
        }

        if child_lived.is_none() {
            match child.try_wait() {
                Ok(Some(_)) => child_lived = Some(started.elapsed()),
                Ok(None) => {}
                Err(_) => return,
            }
        }

        // 真正撑住的 mstsc 已退出，且窗口也不见了
        if let Some(lived) = child_lived {
            if lived >= Duration::from_secs(2) && !found { return; }
        }

        if !seen_window {
            let elapsed = started.elapsed();
            let launcher_only = child_lived.map(|lived| lived < Duration::from_secs(2)).unwrap_or(false);
            if launcher_only && elapsed > Duration::from_secs(20) { return; }
            if elapsed > Duration::from_secs(90) { return; }
        }

        std::thread::sleep(Duration::from_millis(400));
    }
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
    if !is_our_rdp_window(&title, &state.address, &state.desired_title) { return 1; }
    state.found = true;
    if title != state.desired_title {
        let wide_title: Vec<u16> = state
            .desired_title
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe { SetWindowTextW(hwnd, wide_title.as_ptr()); }
    }
    1
}

fn is_our_rdp_window(title: &str, address: &str, desired_title: &str) -> bool {
    if title == desired_title { return true; }
    if !title.contains(address) { return false; }
    GENERIC_TITLE_SUFFIXES.iter().any(|suffix| title.contains(suffix))
        || title.starts_with(&format!("{address} - "))
}

fn window_title(hwnd: WindowHandle) -> String {
    let mut buffer = [0u16; 512];
    let length = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 0 { return String::new(); }
    String::from_utf16_lossy(&buffer[..length as usize])
}
