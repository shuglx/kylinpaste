//! 系统托盘

use tauri::{
    AppHandle, CustomMenuItem, SystemTray, SystemTrayEvent, SystemTrayMenu, SystemTrayMenuItem,
};

pub fn create() -> SystemTray {
    let toggle = CustomMenuItem::new("toggle", "显示/隐藏 (Ctrl+Alt+V)");
    let clear = CustomMenuItem::new("clear", "清空历史");
    let autostart = CustomMenuItem::new("autostart", "开机自启");
    let quit = CustomMenuItem::new("quit", "退出");

    let menu = SystemTrayMenu::new()
        .add_item(toggle)
        .add_item(clear)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(autostart)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(quit);

    SystemTray::new().with_menu(menu)
}

pub fn handle_event(app: &AppHandle, event: SystemTrayEvent) {
    match event {
        // appindicator 限制:Linux 下左键事件未必触发,菜单项是可靠路径
        SystemTrayEvent::LeftClick { .. } => crate::toggle_main_window(app),
        SystemTrayEvent::MenuItemClick { id, .. } => match id.as_str() {
            "toggle" => crate::toggle_main_window(app),
            "clear" => crate::state::clear(),
            "autostart" => {
                let enable = !crate::system::is_autostart();
                match crate::system::set_autostart(enable) {
                    Ok(_) => {
                        let _ = app
                            .tray_handle()
                            .get_item("autostart")
                            .set_selected(enable);
                    }
                    Err(e) => eprintln!("切换开机自启失败: {e}"),
                }
            }
            "quit" => app.exit(0),
            _ => {}
        },
        _ => {}
    }
}
