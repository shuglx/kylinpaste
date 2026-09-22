//! 全局热键注册:Linux 优先用自建 X11 实现(原因见 `x11.rs` 顶部注释),
//! 抓键失败或非 Linux 平台回退 Tauri 内置实现。
//!
//! 两种实现都支持运行时改绑,区别只在"怎么换":
//! 自建实现交给监听线程重新 grab(可同步拿到结果),内置实现只能全清再注册。
//!
//! 另外记录**注册状态**(`status()`),供设置界面提示"没绑上/被占用";
//! 支持"录入期间暂停"(`set_recording`):被动抓键会把按键从 webview 手里截走,
//! 不暂停的话用户在设置里根本录不进当前正在生效的那个组合。

#[allow(unused_imports)] // 只在 Linux 分支里用到(日志)
use crate::klog;
use parking_lot::Mutex;
use tauri::{AppHandle, GlobalShortcutManager};

#[derive(Clone, Copy, PartialEq)]
// X11 变体只在 Linux 上会被创建(mac 预览版恒走内置实现),这里统一放行
#[allow(dead_code)]
enum Backend {
    /// 自建 X11 实现(见 x11.rs)
    X11,
    /// Tauri 内置实现
    Tauri,
}

static BACKEND: Mutex<Option<Backend>> = Mutex::new(None);
static ACCEL: Mutex<String> = Mutex::new(String::new());
/// 设置界面是否正在录快捷键(录入期间热键处于暂停状态)
static RECORDING: Mutex<bool> = Mutex::new(false);

/// 热键当前状态(设置界面展示 + 排查用)
#[derive(Clone, serde::Serialize)]
pub struct HotkeyStatus {
    /// 当前加速键
    pub acceleration: String,
    /// 生效的实现:"X11" / "Tauri 内置" / "无"
    pub backend: String,
    /// 是否可用
    pub ok: bool,
    /// 失败原因或提示
    pub message: String,
    /// 此刻是否处于"设置界面录快捷键"而临时暂停
    pub recording: bool,
    /// 是否 Wayland 会话(X11 全局热键对原生 Wayland 窗口不生效)
    pub wayland: bool,
}

static STATUS: Mutex<Option<HotkeyStatus>> = Mutex::new(None);

/// 当前状态(还没注册过时给一份"未知"的描述)
pub fn status() -> HotkeyStatus {
    let mut status = STATUS.lock().clone().unwrap_or(HotkeyStatus {
        acceleration: ACCEL.lock().clone(),
        backend: "无".into(),
        ok: false,
        message: "尚未注册全局快捷键".into(),
        recording: false,
        wayland: wayland_session(),
    });
    status.recording = *RECORDING.lock();
    status
}

fn set_status(accel: &str, backend: &str, ok: bool, message: String) {
    crate::klog!("[热键] 状态: {accel} | {backend} | ok={ok} | {message}");
    *STATUS.lock() = Some(HotkeyStatus {
        acceleration: accel.to_string(),
        backend: backend.to_string(),
        ok,
        message,
        recording: *RECORDING.lock(),
        wayland: wayland_session(),
    });
}

/// Wayland 会话下 X11 的被动抓键对原生 Wayland 窗口不生效,粘贴注入同理——
/// 抓键"成功"但按了没反应,多半就是这个原因,状态里要提示出来。
fn wayland_session() -> bool {
    std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false)
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}

/// 启动时注册(失败只记日志、不阻塞启动:此时界面还没起来,没法提示用户;
/// 失败原因会留在 `status()` 里,设置界面再提示)
pub fn init(app: &AppHandle, accel: &str) {
    if let Err(e) = apply(app, accel) {
        crate::klog!("[热键] 注册 {accel} 失败(应用继续运行,可在设置里改绑): {e}");
        set_status(accel, "无", false, e);
    }
}

/// 应用热键(启动与设置页改绑都走这里),返回失败原因供界面展示。
pub fn apply(app: &AppHandle, accel: &str) -> Result<(), String> {
    *ACCEL.lock() = accel.to_string();
    // 先把当前后端读出来:下面各分支都要改 BACKEND,
    // 若写成 `match *BACKEND.lock() {..}` 守卫会活到整个 match 结束,同一个 Mutex 再锁就死锁
    let backend = *BACKEND.lock();
    match backend {
        Some(Backend::X11) => match set_own(accel) {
            Ok(()) => {
                set_status(accel, "X11", true, backend_note("X11"));
                Ok(())
            }
            Err(e) => {
                set_status(accel, "X11", false, e.clone());
                Err(e)
            }
        },
        Some(Backend::Tauri) => register_builtin(app, accel),
        None => {
            if start_own(app, accel) {
                set_status(accel, "X11", true, backend_note("X11"));
                Ok(())
            } else {
                register_builtin(app, accel).map_err(|e| {
                    set_status(accel, "无", false, e.clone());
                    e
                })
            }
        }
    }
}

/// 成功时也给一句提示:Wayland 会话要提前说明"可能按了没反应"的原因
fn backend_note(backend: &str) -> String {
    if wayland_session() {
        format!("{backend} 已注册。注意:当前是 Wayland 会话,X11 全局热键对原生 Wayland 窗口不生效")
    } else {
        format!("{backend} 已注册")
    }
}

/// 第一次:把自建监听线程拉起来(线程里带回调,只需建一次)
fn start_own(app: &AppHandle, accel: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        let handle = app.clone();
        let callback = move || crate::toggle_main_window(&handle);
        match crate::x11::start_hotkey_listener(accel, callback) {
            Ok(()) => {
                *BACKEND.lock() = Some(Backend::X11);
                true
            }
            Err(e) => {
                crate::klog!("[热键] 自建实现不可用({e}),回退 Tauri 内置实现");
                false
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, accel);
        false
    }
}

/// 已用自建实现 → 让监听线程先抓新的、成功后再放旧的
fn set_own(accel: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        crate::x11::set_hotkey(accel)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = accel;
        Err("自建全局热键仅在 Linux 可用".to_string())
    }
}

fn register_builtin(app: &AppHandle, accel: &str) -> Result<(), String> {
    let mut manager = app.global_shortcut_manager();
    // 改绑:内置实现没有按加速键注销的接口,直接全清再注册
    let _ = manager.unregister_all();
    let handle = app.clone();
    manager
        .register(accel, move || crate::toggle_main_window(&handle))
        .map_err(|e| format!("注册热键 {accel} 失败(可能被别的程序占用): {e}"))?;
    *BACKEND.lock() = Some(Backend::Tauri);
    set_status(accel, "Tauri 内置", true, backend_note("Tauri 内置"));
    Ok(())
}

/// 设置界面录快捷键期间暂停/恢复全局热键。
///
/// 不暂停的话:被动抓键会把该组合的按键事件从 webview 手里截走,
/// 用户在设置里按什么都不会被录入界面看到("无法绑定"的根因)。
pub fn set_recording(app: &AppHandle, recording: bool) {
    *RECORDING.lock() = recording;
    let backend = *BACKEND.lock();
    match backend {
        Some(Backend::X11) => {
            let result: Result<(), String> = if recording {
                #[cfg(target_os = "linux")]
                {
                    crate::x11::pause_hotkey()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Ok(())
                }
            } else {
                #[cfg(target_os = "linux")]
                {
                    crate::x11::resume_hotkey()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Ok(())
                }
            };
            if let Err(e) = result {
                crate::klog!("[热键] 录入期间{}失败: {e}", if recording { "暂停" } else { "恢复" });
            }
        }
        Some(Backend::Tauri) => {
            // 内置实现只能注销/重注册
            if recording {
                let _ = app.global_shortcut_manager().unregister_all();
            } else {
                let accel = ACCEL.lock().clone();
                if !accel.is_empty() {
                    if let Err(e) = register_builtin(app, &accel) {
                        crate::klog!("[热键] 录入结束恢复失败: {e}");
                    }
                }
            }
        }
        None => {}
    }
    crate::klog!("[热键] 录入模式: {recording}");
}
