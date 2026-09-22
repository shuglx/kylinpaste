//! 来源应用识别:把"这条剪贴板内容是从哪个程序复制来的"变成一个可展示的名字。
//!
//! - macOS:`NSWorkspace.frontmostApplication.localizedName`(如 `CodeBuddy CN`、`Safari`)
//! - Linux/X11:活动窗口的 `WM_CLASS`(如 `firefox`、`Wps`、`XTerm`),见 x11.rs
//!
//! 识别不出来(平台不支持、没有活动窗口、前台就是本应用)时返回 None,
//! 前端副标题退化成只显示类型,不会出现空占位。

// objc 0.2 的 msg_send!/sel_impl 内部用了 #[cfg(feature = "cargo-clippy")],
// 新版 rustc 会对这个"不存在的 feature"报 unexpected_cfgs 警告(宏展开处无法单独标注)
#![allow(unexpected_cfgs)]

/// 复制动作发生时前台应用的显示名
pub fn active_app_name() -> Option<String> {
    let name = platform::frontmost_app_name()?;
    let name = name.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CStr;

    /// 把 NSString 转成 Rust String(id 为空时返回 None)
    unsafe fn nsstring_to_string(ns: *mut Object) -> Option<String> {
        if ns.is_null() {
            return None;
        }
        let utf8: *const std::os::raw::c_char = msg_send![ns, UTF8String];
        if utf8.is_null() {
            return None;
        }
        Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
    }

    /// 前台应用名。NSWorkspace 可从任意线程访问(剪贴板监听线程也会调用)。
    pub fn frontmost_app_name() -> Option<String> {
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace.is_null() {
                return None;
            }
            let app: *mut Object = msg_send![workspace, frontmostApplication];
            if app.is_null() {
                return None;
            }

            // 前台是本应用时不算来源(说明这个复制动作发生在我们自己的窗口里)。
            // 用 pid 判断而不是 bundle id:开发模式下直接跑 target/debug 里的可执行文件,
            // mainBundle 没有 bundle id,只有 pid 两种情况都靠得住。
            let pid: i32 = msg_send![app, processIdentifier];
            if pid > 0 && pid as u32 == std::process::id() {
                return None;
            }

            nsstring_to_string(msg_send![app, localizedName])
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    pub fn frontmost_app_name() -> Option<String> {
        crate::x11::active_app_name()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    pub fn frontmost_app_name() -> Option<String> {
        None
    }
}
