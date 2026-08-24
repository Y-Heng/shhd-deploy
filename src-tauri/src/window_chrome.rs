//! 将原生标题栏、边框与窗口底色对齐到应用主题，避免系统灰条和主体脱节。

use tauri::{AppHandle, Manager, Theme, WebviewWindow, window::Color};

/// 与 theme.css 中 --app-bg 保持一致
const DARK_BG: Color = Color(15, 18, 24, 255);
const LIGHT_BG: Color = Color(238, 241, 246, 255);

/// 按当前深浅色刷新所有窗口的原生边框颜色
#[tauri::command]
pub fn apply_window_chrome(app: AppHandle, dark: bool) {
    for window in app.webview_windows().values() {
        apply_to_window(window, dark);
    }
}

fn apply_to_window(window: &WebviewWindow, dark: bool) {
    let theme = if dark { Theme::Dark } else { Theme::Light };
    let _ = window.set_theme(Some(theme));
    let background = if dark { DARK_BG } else { LIGHT_BG };
    let _ = window.set_background_color(Some(background));
    #[cfg(windows)]
    apply_dwm_caption(window, dark);
}

/// Windows COLORREF：0x00BBGGRR
#[cfg(windows)]
fn rgb_to_colorref(red: u8, green: u8, blue: u8) -> u32 {
    u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16)
}

#[cfg(windows)]
fn apply_dwm_caption(window: &WebviewWindow, dark: bool) {
    let Ok(hwnd) = window.hwnd() else { return };
    let caption = if dark {
        rgb_to_colorref(15, 18, 24)
    } else {
        rgb_to_colorref(238, 241, 246)
    };
    let text = if dark {
        rgb_to_colorref(215, 221, 232)
    } else {
        rgb_to_colorref(30, 37, 48)
    };
    let immersive_dark: i32 = if dark { 1 } else { 0 };
    unsafe {
        dwm_set_attr(hwnd.0, DWMWA_USE_IMMERSIVE_DARK_MODE, &immersive_dark);
        dwm_set_attr(hwnd.0, DWMWA_BORDER_COLOR, &caption);
        dwm_set_attr(hwnd.0, DWMWA_CAPTION_COLOR, &caption);
        dwm_set_attr(hwnd.0, DWMWA_TEXT_COLOR, &text);
    }
}

#[cfg(windows)]
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
#[cfg(windows)]
const DWMWA_BORDER_COLOR: u32 = 34;
#[cfg(windows)]
const DWMWA_CAPTION_COLOR: u32 = 35;
#[cfg(windows)]
const DWMWA_TEXT_COLOR: u32 = 36;

#[cfg(windows)]
#[link(name = "dwmapi")]
extern "system" {
    fn DwmSetWindowAttribute(
        hwnd: *mut std::ffi::c_void,
        attribute: u32,
        value: *const std::ffi::c_void,
        size: u32,
    ) -> i32;
}

#[cfg(windows)]
unsafe fn dwm_set_attr<T>(hwnd: *mut std::ffi::c_void, attribute: u32, value: &T) {
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            value as *const T as *const std::ffi::c_void,
            std::mem::size_of::<T>() as u32,
        );
    }
}
