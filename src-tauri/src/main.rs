#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appinfo;
mod clipboard_service;
mod hotkey;
mod logfile;
mod paste;
mod settings;
mod state;
mod system;
#[cfg(target_os = "linux")]
mod x11;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::{Manager, WindowEvent};

// cmd_copy_log_path 里要调 ClipboardContext::set_text(方法来自这个 trait)
use clipboard_rs::Clipboard;

/// 连续两次切换的最小间隔(毫秒):多个热键来源或键盘重复时不至于"闪一下又消失"
const TOGGLE_DEBOUNCE_MS: u64 = 250;
static LAST_TOGGLE_MS: AtomicU64 = AtomicU64::new(0);

// 主窗口"可见 / 有焦点"的镜像状态。
//
// **为什么不直接问窗口**:`Window::is_visible()/is_focused()` 在 Linux 上会直接读 GTK
// 内部状态,而调用它们的都是工作线程(热键监听线程、粘贴命令线程),GTK 不是线程安全的:
// 麒麟上表现为"按热键/点条目粘贴时直接崩溃"。所以一律只读自己维护的原子量,
// 真值只在主线程(show/hide 与 Focused 事件)里更新。
static WINDOW_VISIBLE: AtomicBool = AtomicBool::new(false);
static WINDOW_FOCUSED: AtomicBool = AtomicBool::new(false);

/// 主窗口当前是否可见(镜像值,可在任意线程安全读取)
pub fn window_visible() -> bool {
    WINDOW_VISIBLE.load(Ordering::SeqCst)
}

/// 主窗口当前是否有焦点(镜像值)
pub fn window_focused() -> bool {
    WINDOW_FOCUSED.load(Ordering::SeqCst)
}

fn main() {
    // 目标机无 a11y DBus 服务时 GTK 会刷 dbind-WARNING,无害但吓人,提前关闭
    std::env::set_var("NO_AT_BRIDGE", "1");

    // 抢在应用窗口出现之前记下"当前活动窗口":应用启动后主窗口是可见的,
    // 用户不按热键、直接点选条目时,这就是最合理的粘贴落点。
    #[cfg(target_os = "linux")]
    x11::remember_target_window();

    tauri::Builder::default()
        .on_window_event(|event| match event.event() {
            // 窗口没有标题栏,正常关不掉;这里只做防御:请求关闭时收起窗口而不是退出
            WindowEvent::CloseRequested { api, .. } => {
                if event.window().label() == "main" {
                    api.prevent_close();
                    hide_main(&event.window().app_handle());
                }
            }
            // 焦点真值只在这里更新(主线程)
            WindowEvent::Focused(focused) => {
                if event.window().label() == "main" {
                    WINDOW_FOCUSED.store(*focused, Ordering::SeqCst);
                }
            }
            WindowEvent::Destroyed => {
                if event.window().label() == "main" {
                    WINDOW_VISIBLE.store(false, Ordering::SeqCst);
                    WINDOW_FOCUSED.store(false, Ordering::SeqCst);
                }
            }
            _ => {}
        })
        .setup(|app| {
            // 日志要最先起来:启动阶段的失败现场(以及 panic)都要落盘
            if let Some(dir) = app.path_resolver().app_data_dir() {
                logfile::init(dir);
            }
            logfile::install_panic_hook();
            klog!("[启动] setup 开始");
            logfile::log_environment();

            // 单实例检查:确认已有活实例(而非陈旧 socket)才退出
            if !system::acquire_single_instance(app.handle().clone()) {
                klog!("[启动] 检测到已有实例,已请求其显示窗口,本进程退出");
                std::process::exit(0);
            }

            // 设置要在历史和热键之前载入:保留条数上限、热键都要用它的值
            settings::start(&app.handle());

            // 载入上次的历史(落盘文件),再启动剪贴板监听
            state::start_persistence(&app.handle());

            // 全局热键唤起/隐藏主窗口(失败不致命:原因会记在 hotkey::status 里,
            // 设置界面会提示,用户也可以改绑)
            // Linux 优先用自建实现(原因见 x11.rs 顶部注释),抓键失败回退 Tauri 内置实现
            hotkey::init(&app.handle(), &settings::get().hotkey);

            clipboard_service::start(app.handle().clone())?;
            klog!("[启动] 剪贴板监听已启动");

            // 自检:缩略图要能被前端通过 asset 协议读到(scope 不匹配的话列表只会显示类型图标)
            if let Some(dir) = app.path_resolver().app_data_dir() {
                let sample = dir.join(state::THUMBS_DIR).join("sample.png");
                klog!(
                    "[资源] 缩略图 {} 在 asset scope 内: {}",
                    sample.display(),
                    app.asset_protocol_scope().is_allowed(&sample)
                );
            }

            // 防御性确保主窗口可见(顺带补一次聚焦,首启时用户可以直接输入/按数字键)
            let handle = app.handle();
            show_main(&handle);

            klog!("[启动] 完成");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            state::cmd_get_history,
            state::cmd_set_group,
            state::cmd_set_favorite,
            state::cmd_import_favorites,
            state::cmd_delete_records,
            state::cmd_get_asset_dirs,
            paste::cmd_paste_item,
            settings::cmd_get_settings,
            settings::cmd_set_settings,
            system::cmd_get_autostart,
            system::cmd_set_autostart,
            cmd_get_app_info,
            cmd_set_always_on_top,
            cmd_hide_main,
            cmd_quit_app,
            cmd_log,
            cmd_get_log_path,
            cmd_copy_log_path,
            cmd_get_hotkey_status,
            cmd_set_hotkey_recording,
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
    WINDOW_VISIBLE.store(false, Ordering::SeqCst);
    WINDOW_FOCUSED.store(false, Ordering::SeqCst);
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
        klog!("[窗口] 置顶设置失败: {e}");
        return Err(e.to_string());
    }
    // Linux 再向窗口管理器直接发一遍 EWMH:GTK 的 keep_above 在 UKUI 等桌面
    // 上不一定被理会,置顶不生效的表现就是"点了别处窗口就被压到后面"
    #[cfg(target_os = "linux")]
    {
        let via_ewmh = x11::set_above(enabled);
        klog!("[窗口] 置顶={enabled}(GTK ✓,EWMH 请求={via_ewmh})");
    }
    #[cfg(not(target_os = "linux"))]
    klog!("[窗口] 置顶={enabled}");
    Ok(enabled)
}

/// 隐藏主窗口(窗口没有标题栏,Esc 用它收起来)
#[tauri::command]
fn cmd_hide_main(app: tauri::AppHandle) {
    hide_main(&app);
}

/// 应用名与版本(设置界面"关于"页展示)
#[derive(serde::Serialize)]
struct AppInfo {
    name: String,
    version: String,
}

#[tauri::command]
fn cmd_get_app_info(app: tauri::AppHandle) -> AppInfo {
    let pkg = app.package_info();
    AppInfo {
        name: pkg.name.clone(),
        version: pkg.version.to_string(),
    }
}

/// 退出应用。去掉托盘之后,这是唯一的退出入口(设置界面 → 关于)。
#[tauri::command]
fn cmd_quit_app(app: tauri::AppHandle) {
    klog!("[启动] 用户从设置界面退出应用");
    app.exit(0);
}

/// 前端往日志里写一行(比如 WebView 的 UA:用来确认 WebKitGTK 版本,判断 CSS 支持范围)
#[tauri::command]
fn cmd_log(message: String) {
    klog!("[前端] {message}");
}

/// 日志文件路径(设置界面"关于"页可以显示出来,方便用户拷给开发者)
#[tauri::command]
fn cmd_get_log_path() -> Option<String> {
    logfile::path().map(|p| p.to_string_lossy().into_owned())
}

/// 把日志文件路径写进剪贴板(界面上的"复制路径"按钮;webview 里的
/// navigator.clipboard 不可靠,走内核的剪贴板写入)
#[tauri::command]
fn cmd_copy_log_path() -> Result<String, String> {
    let path = logfile::path().ok_or("日志还没初始化")?;
    let text = path.to_string_lossy().into_owned();
    clipboard_service::with_writer(|ctx| {
        ctx.set_text(text.clone()).map_err(|e| format!("写入剪贴板失败: {e}"))
    })?;
    Ok(text)
}

/// 全局热键的注册状态(设置界面据此提示"没绑上/被占用/是 Wayland")
#[tauri::command]
fn cmd_get_hotkey_status() -> hotkey::HotkeyStatus {
    hotkey::status()
}

/// 设置界面开始/结束录快捷键:期间暂停全局热键,
/// 否则被动抓键会把按键事件从 webview 手里截走,当前生效的组合永远录不进去
#[tauri::command]
fn cmd_set_hotkey_recording(app: tauri::AppHandle, recording: bool) {
    hotkey::set_recording(&app, recording);
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
    WINDOW_VISIBLE.store(true, Ordering::SeqCst);

    #[cfg(target_os = "linux")]
    {
        // UKUI 等桌面对 tao/GTK 的 set_focus 有"防抢焦点"式忽略:窗口已经映射时
        // set_focus 是空操作,表现就是"窗口在跑但按热键呼不到最前"。
        // 所以等窗口进入 _NET_CLIENT_LIST 后,直接发 EWMH 激活(source=2 可绕过限制),
        // 由 activate_window 内部兜底 XSetInputFocus。
        std::thread::spawn(move || {
            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(80));
                if !window_visible() {
                    return;
                }
                if let Some(xid) = x11::main_window_xid() {
                    if x11::activate_window(xid) {
                        klog!("[窗口] 已通过 EWMH 前置并聚焦主窗口");
                        return;
                    }
                }
            }
            klog!("[窗口] EWMH 前置主窗口未成功(桌面可能未响应激活请求)");
        });
    }

    #[cfg(not(target_os = "linux"))]
    {
        // show() 只是投递一个窗口请求,而 tao 的 set_focus() 会检查"窗口当前是否可见",
        // 紧跟着调用时窗口还没映射,聚焦请求会被直接丢弃:窗口虽然被映射出来,
        // 却拿不到焦点(还可能压在别的窗口后面),用户得再按一次热键才"真的出来"。
        // 这里用"自己记的可见状态 + 稍等再聚焦"代替轮询 is_visible()(工作线程读 GTK 会崩)。
        let handle = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            for _ in 0..8 {
                if window_focused() || !window_visible() {
                    return;
                }
                let focus_handle = handle.clone();
                let _ = handle.run_on_main_thread(move || {
                    if let Some(win) = focus_handle.get_window("main") {
                        let _ = win.set_focus();
                    }
                });
                std::thread::sleep(std::time::Duration::from_millis(60));
            }
            klog!("[窗口] 等待窗口获得焦点超时,未能补发聚焦请求");
        });
    }
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

    let visible = window_visible();
    // Linux:活动窗口以 X11 的 _NET_ACTIVE_WINDOW 为准 —— 镜像的焦点状态依赖 GTK
    // 的 Focused 事件,UKUI 这类桌面收不到,会让切换走错分支(该前置时什么都不做)。
    // 可见但不是活动窗口 → 前置并聚焦;是活动窗口 → 隐藏;不可见 → 显示。
    #[cfg(target_os = "linux")]
    let focused = x11::main_window_is_active();
    #[cfg(not(target_os = "linux"))]
    let focused = window_focused();
    if visible && focused {
        klog!("[窗口] 热键切换: 可见={visible} 是活动窗口={focused} → 隐藏");
        hide_main(app);
    } else {
        klog!("[窗口] 热键切换: 可见={visible} 是活动窗口={focused} → 显示并前置");
        show_main(app);
    }
}
