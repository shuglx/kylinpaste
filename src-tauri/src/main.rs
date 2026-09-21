#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard_service;
mod paste;
mod state;
mod system;
mod tray;

use tauri::{GlobalShortcutManager, Manager, WindowEvent};

fn main() {
    // 目标机无 a11y DBus 服务时 GTK 会刷 dbind-WARNING,无害但吓人,提前关闭
    std::env::set_var("NO_AT_BRIDGE", "1");

    tauri::Builder::default()
        .system_tray(tray::create())
        .on_system_tray_event(tray::handle_event)
        .on_window_event(|event| {
            // 关闭主窗口时隐藏到托盘而不是退出
            if let WindowEvent::CloseRequested { api, .. } = event.event() {
                if event.window().label() == "main" {
                    api.prevent_close();
                    hide_main(&event.window().app_handle());
                }
            }
        })
        .setup(|app| {
            eprintln!("[启动] setup 开始");

            // 单实例检查:确认已有活实例(而非陈旧 socket)才退出
            if !system::acquire_single_instance(app.handle().clone()) {
                eprintln!("[启动] 检测到已有实例,已请求其显示窗口,本进程退出");
                std::process::exit(0);
            }

            // 全局快捷键 Ctrl+Alt+V 唤起/隐藏主窗口(失败不致命,降级继续)
            let handle = app.handle().clone();
            if let Err(e) = app.global_shortcut_manager().register("Ctrl+Alt+V", move || {
                toggle_main_window(&handle);
            }) {
                eprintln!("[启动] 注册全局快捷键 Ctrl+Alt+V 失败(应用继续运行,可从托盘唤起): {e}");
            } else {
                eprintln!("[启动] 全局快捷键已注册");
            }

            // 托盘菜单勾选状态与自启状态同步
            let _ = app
                .tray_handle()
                .get_item("autostart")
                .set_selected(system::is_autostart());

            // 启动剪贴板监听
            clipboard_service::start(app.handle().clone())?;
            eprintln!("[启动] 剪贴板监听已启动");

            // 防御性确保主窗口可见
            if let Some(win) = app.get_window("main") {
                let _ = win.show();
            }

            eprintln!("[启动] 完成");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            state::cmd_get_history,
            state::cmd_clear_history,
            paste::cmd_paste_item,
            system::cmd_get_autostart,
            system::cmd_set_autostart,
        ])
        .run(tauri::generate_context!())
        .expect("KylinPaste 运行失败");
}

/// 隐藏主窗口(隐藏到托盘)
///
/// macOS 下仅隐藏窗口时应用仍处于激活态且没有键窗口:
/// 模拟按键会被系统丢弃、焦点也不会交还目标应用,必须隐藏整个应用;
/// X11 下取消映射窗口即可,窗口管理器自动把焦点还给上一个窗口。
pub fn hide_main(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.hide();
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(win) = app.get_window("main") {
            let _ = win.hide();
        }
    }
}

/// 显示/隐藏主窗口
pub fn toggle_main_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_window("main") {
        let visible = win.is_visible().unwrap_or(false);
        let focused = win.is_focused().unwrap_or(false);
        // 可见且持有焦点 → 隐藏;
        // 可见但失焦(被遮挡/应用在后台)→ 前置并聚焦(避免"按一次没反应"的错觉);
        // 隐藏 → 显示
        if visible && focused {
            hide_main(app);
        } else {
            // macOS:应用可能整体处于隐藏状态,先恢复再显示
            #[cfg(target_os = "macos")]
            let _ = app.show();
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}
