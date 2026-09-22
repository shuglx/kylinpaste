#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appinfo;
mod clipboard_service;
mod paste;
mod state;
mod system;
mod tray;
#[cfg(target_os = "linux")]
mod x11;

use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{GlobalShortcutManager, Manager, WindowEvent};

/// 连续两次切换的最小间隔(毫秒):多个热键来源或键盘重复时不至于"闪一下又消失"
const TOGGLE_DEBOUNCE_MS: u64 = 250;
static LAST_TOGGLE_MS: AtomicU64 = AtomicU64::new(0);

fn main() {
    // 目标机无 a11y DBus 服务时 GTK 会刷 dbind-WARNING,无害但吓人,提前关闭
    std::env::set_var("NO_AT_BRIDGE", "1");

    // 抢在应用窗口出现之前记下"当前活动窗口":应用启动后主窗口是可见的,
    // 用户不按热键、直接点选条目时,这就是最合理的粘贴落点。
    #[cfg(target_os = "linux")]
    x11::remember_target_window();

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
            // Linux 优先用自建实现(原因见 x11.rs 顶部注释),抓键失败再回退 Tauri 内置实现
            let hotkey_handle = app.handle().clone();
            #[cfg(target_os = "linux")]
            let own_hotkey = match x11::start_hotkey_listener({
                let handle = hotkey_handle.clone();
                move || toggle_main_window(&handle)
            }) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("[启动] 自建全局热键不可用({e}),回退 Tauri 内置实现");
                    false
                }
            };
            #[cfg(not(target_os = "linux"))]
            let own_hotkey = false;

            if !own_hotkey {
                if let Err(e) = app
                    .global_shortcut_manager()
                    .register("Ctrl+Alt+V", move || toggle_main_window(&hotkey_handle))
                {
                    eprintln!("[启动] 注册全局快捷键 Ctrl+Alt+V 失败(应用继续运行,可从托盘唤起): {e}");
                } else {
                    eprintln!("[启动] 全局快捷键已注册(Tauri 内置实现)");
                }
            }

            // 托盘菜单勾选状态与自启状态同步
            let _ = app
                .tray_handle()
                .get_item("autostart")
                .set_selected(system::is_autostart());

            // 载入上次的历史(落盘文件),再启动剪贴板监听
            state::start_persistence(&app.handle());

            clipboard_service::start(app.handle().clone())?;
            eprintln!("[启动] 剪贴板监听已启动");

            // 自检:缩略图要能被前端通过 asset 协议读到(scope 不匹配的话列表只会显示类型图标)
            if let Some(dir) = app.path_resolver().app_data_dir() {
                let sample = dir.join(state::THUMBS_DIR).join("sample.png");
                println!(
                    "[资源] 缩略图 {} 在 asset scope 内: {}",
                    sample.display(),
                    app.asset_protocol_scope().is_allowed(&sample)
                );
            }

            // 防御性确保主窗口可见(顺带补一次聚焦,首启时用户可以直接输入/按数字键)
            let handle = app.handle();
            show_main(&handle);

            eprintln!("[启动] 完成");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            state::cmd_get_history,
            state::cmd_clear_history,
            state::cmd_set_group,
            state::cmd_delete_records,
            state::cmd_get_asset_dirs,
            paste::cmd_paste_item,
            system::cmd_get_autostart,
            system::cmd_set_autostart,
            cmd_set_always_on_top,
            cmd_hide_main,
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

/// 置顶(界面上的图钉):让主窗口始终浮在其它窗口之上
#[tauri::command]
fn cmd_set_always_on_top(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    let win = app.get_window("main").ok_or("主窗口不存在")?;
    if let Err(e) = win.set_always_on_top(enabled) {
        eprintln!("[窗口] 置顶设置失败: {e}");
        return Err(e.to_string());
    }
    println!("[窗口] 置顶={enabled}");
    Ok(enabled)
}

/// 隐藏主窗口(窗口没有标题栏,Esc 用它收起来)
#[tauri::command]
fn cmd_hide_main(app: tauri::AppHandle) {
    hide_main(&app);
}

/// 显示主窗口并抢到焦点(粘贴时需要把焦点还给"上一个活动窗口",所以先记下来)
pub fn show_main(app: &tauri::AppHandle) {
    #[cfg(target_os = "linux")]
    x11::remember_target_window();

    // macOS:应用可能整体处于隐藏状态,先恢复再显示
    #[cfg(target_os = "macos")]
    let _ = app.show();

    if let Some(win) = app.get_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
    }

    // show() 只是投递一个窗口请求,而 tao 的 set_focus() 会检查"窗口当前是否可见",
    // 紧跟着调用时窗口还没映射,聚焦请求会被直接丢弃:窗口虽然被映射出来,
    // 却拿不到焦点(还可能压在别的窗口后面),用户得再按一次热键才"真的出来"。
    // 所以等窗口真正可见之后再聚焦一次(在独立线程里做,避免阻塞调用方)。
    let handle = app.clone();
    std::thread::spawn(move || {
        for _ in 0..40 {
            match handle.get_window("main") {
                Some(win) if win.is_visible().unwrap_or(false) => {
                    let _ = win.set_focus();
                    return;
                }
                Some(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
                None => return,
            }
        }
        eprintln!("[窗口] 等待窗口可见超时,未能补发聚焦请求");
    });
}

/// 显示/隐藏主窗口
pub fn toggle_main_window(app: &tauri::AppHandle) {
    // 去抖:忽略键盘重复、或多个热键来源造成的连续触发
    let now = state::now_ms();
    let last = LAST_TOGGLE_MS.load(Ordering::SeqCst);
    if now.saturating_sub(last) < TOGGLE_DEBOUNCE_MS {
        return;
    }
    LAST_TOGGLE_MS.store(now, Ordering::SeqCst);

    let Some(win) = app.get_window("main") else {
        return;
    };
    let visible = win.is_visible().unwrap_or(false);
    let focused = win.is_focused().unwrap_or(false);
    // 可见且持有焦点 → 隐藏;
    // 可见但失焦(被遮挡/应用在后台)→ 前置并聚焦(避免"按一次没反应"的错觉);
    // 隐藏 → 显示
    if visible && focused {
        println!("[窗口] 切换: 可见={visible} 有焦点={focused} → 隐藏");
        hide_main(app);
    } else {
        println!("[窗口] 切换: 可见={visible} 有焦点={focused} → 显示并聚焦");
        show_main(app);
    }
}
