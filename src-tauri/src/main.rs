//! 桌面应用入口。Release 构建在 Windows 下隐藏控制台窗口。

// 隐藏 Windows 下的控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kurumi_deploy_lib::run()
}
