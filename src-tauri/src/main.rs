// 隐藏 Windows 下的控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kurumi_deploy_lib::run()
}
