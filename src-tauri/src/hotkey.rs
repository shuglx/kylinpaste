//! 全局热键注册:Linux 优先用自建 X11 实现(原因见 `x11.rs` 顶部注释),
//! 抓键失败或非 Linux 平台回退 Tauri 内置实现。
//!
//! 两种实现都支持运行时改绑,区别只在"怎么换":
//! 自建实现交给监听线程重新 grab(可同步拿到结果),内置实现只能全清再注册。

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

/// 启动时注册(失败只记日志、不阻塞启动:此时界面还没起来,没法提示用户)
pub fn init(app: &AppHandle, accel: &str) {
    if let Err(e) = apply(app, accel) {
        eprintln!("[热键] 注册 {accel} 失败(应用继续运行,可在设置里改绑): {e}");
    }
}

/// 应用热键(启动与设置页改绑都走这里),返回失败原因供界面展示。
pub fn apply(app: &AppHandle, accel: &str) -> Result<(), String> {
    // 先把当前后端读出来:下面各分支都要改 BACKEND,
    // 若写成 `match *BACKEND.lock() {..}` 守卫会活到整个 match 结束,同一个 Mutex 再锁就死锁
    let backend = *BACKEND.lock();
    match backend {
        Some(Backend::X11) => set_own(accel),
        Some(Backend::Tauri) => register_builtin(app, accel),
        None => {
            if start_own(app, accel) {
                Ok(())
            } else {
                register_builtin(app, accel)
            }
        }
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
                eprintln!("[热键] 自建实现不可用({e}),回退 Tauri 内置实现");
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
        .map_err(|e| format!("注册热键 {accel} 失败: {e}"))?;
    *BACKEND.lock() = Some(Backend::Tauri);
    println!("[热键] 已注册 {accel}(Tauri 内置实现)");
    Ok(())
}