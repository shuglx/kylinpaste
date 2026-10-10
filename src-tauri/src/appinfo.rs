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
    use once_cell::sync::Lazy;
    use parking_lot::Mutex;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CStr;

    type AXUIElementRef = *mut std::os::raw::c_void;
    type CFStringRef = *const std::os::raw::c_void;

    // AX(Accessibility)能力在 ApplicationServices/HIServices 里;与 enigo 共用
    // 同一个"辅助功能"权限开关(没授权时 API 静默报错,不影响主链路)
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut *mut std::os::raw::c_void,
        ) -> i32;
        fn AXUIElementSetAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut std::os::raw::c_void,
        ) -> i32;
        fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> i32;
        /// AXValue 包装类型:创建/解析(选区范围 kAXSelectedTextRangeAttribute 用)
        fn AXValueCreate(value_type: u32, value_ptr: *const std::os::raw::c_void) -> AXUIElementRef;
        fn AXValueGetValue(
            value: AXUIElementRef,
            value_type: u32,
            value_ptr: *mut std::os::raw::c_void,
        ) -> bool;
        /// 问一个 AXValue 实际是什么类型(自己猜类型解析会在类型不对时静默失败)
        fn AXValueGetType(value: AXUIElementRef) -> u32;
        fn CFRelease(cf: *const std::os::raw::c_void);

        fn CFStringCreateWithCString(
            alloc: *const std::os::raw::c_void,
            c_str: *const std::os::raw::c_char,
            encoding: u32,
        ) -> CFStringRef;
    }

    /// CFRange(与 CoreFoundation 的 CFIndex 对齐,64 位下是 isize)
    #[repr(C)]
    struct CFRange {
        location: isize,
        length: isize,
    }

    /// null 安全的 CFRelease:CFRelease(NULL) 会直接 abort(Crash 报告见
    /// "Electron 应用 AXFocusedUIElement 拿不到 → 记录里 element 为空"那一次)
    unsafe fn cf_release(ptr: *const std::os::raw::c_void) {
        if !ptr.is_null() {
            CFRelease(ptr);
        }
    }

    const KCF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const KAX_ERROR_SUCCESS: i32 = 0;

    // CFBooleanTrue 是 CoreFoundation 真实导出的符号,可直接 extern;
    // 而 kAX* 属性名在新 SDK 里是宏(CFSTR("AXFocusedWindow") 这类)而非可链接
    // 符号,extern 声明会链接失败——按字符串内容自建等价 CFString。
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFBooleanTrue: *const std::os::raw::c_void;
        fn CFEqual(a: *const std::os::raw::c_void, b: *const std::os::raw::c_void) -> bool;
        fn CFStringGetLength(string: CFStringRef) -> isize;
        fn CFStringGetCString(
            string: CFStringRef,
            buffer: *mut std::os::raw::c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> bool;
    }

    /// 临时建一个属性名 CFString,用完即释放(每次粘贴创建几个短字符串,开销可忽略)
    unsafe fn with_cf_attr<R>(name: &str, f: impl FnOnce(CFStringRef) -> R) -> R {
        let c = std::ffi::CString::new(name).expect("属性名不含 \\0");
        let attr = CFStringCreateWithCString(
            std::ptr::null(),
            c.as_ptr(),
            KCF_STRING_ENCODING_UTF8,
        );
        let result = f(attr);
        CFRelease(attr);
        result
    }

    /// 读 AX 元素的字符串属性(标题/角色等,日志与校验用)
    unsafe fn ax_string(element: AXUIElementRef, name: &str) -> Option<String> {
        with_cf_attr(name, |attr| {
            let mut value: *mut std::os::raw::c_void = std::ptr::null_mut();
            if AXUIElementCopyAttributeValue(element, attr, &mut value) != KAX_ERROR_SUCCESS
                || value.is_null()
            {
                return None;
            }
            let len = CFStringGetLength(value as CFStringRef);
            let mut buf = vec![0 as std::os::raw::c_char; (len + 1) as usize * 4];
            let ok = CFStringGetCString(
                value as CFStringRef,
                buf.as_mut_ptr(),
                buf.len() as isize,
                KCF_STRING_ENCODING_UTF8,
            );
            CFRelease(value);
            if ok {
                Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
            } else {
                None
            }
        })
    }

    /// 读元素的选区范围(光标位置),返回 (位置, 长度, AXValue 实际类型)。
    ///
    /// 类型不硬编码:实测该应用返回的 AXValue 类型与头文件枚举的 kAXValueRangeType
    /// 对不上,猜类型解析/创建都会静默失败,必须记下实际类型、写回时原样使用。
    /// 不支持该属性的元素返回 None(err 也打日志,便于区分"属性不支持"与"解析失败")。
    unsafe fn ax_selected_range(element: AXUIElementRef) -> Option<(isize, isize, u32)> {
        with_cf_attr("AXSelectedTextRange", |attr| {
            let mut value: *mut std::os::raw::c_void = std::ptr::null_mut();
            let err = AXUIElementCopyAttributeValue(element, attr, &mut value);
            if err != KAX_ERROR_SUCCESS || value.is_null() {
                crate::klog!("[粘贴] 读选区失败: err={err}");
                return None;
            }
            let actual_type = AXValueGetType(value);
            let mut range = CFRange { location: 0, length: 0 };
            let ok = AXValueGetValue(
                value,
                actual_type,
                &mut range as *mut CFRange as *mut std::os::raw::c_void,
            );
            cf_release(value);
            if !ok {
                crate::klog!("[粘贴] 读选区失败: AXValue 解析失败(类型={actual_type})");
                return None;
            }
            Some((range.location, range.length, actual_type))
        })
    }

    /// 呼出剪贴板前记下的"前台应用 + 它当时的焦点窗口 + 焦点控件"。
    ///
    /// 窗口级对付"多窗口应用激活时挑错窗口";控件级(AXFocusedUIElement,即正文
    /// 输入框这类最深层的焦点元素)对付"写信界面是主窗口内嵌页"的应用——
    /// 后者窗口级恢复等于没动,只有把控件焦点设回去光标才会回来。
    /// sel 是控件当时的选区(光标位置):恢复 AXFocused 会让应用把光标重置到开头,
    /// 所以要原样写回,⌘V 才落在用户原本的位置。
    struct RememberedFocus {
        pid: i32,
        window: *mut std::os::raw::c_void,
        element: *mut std::os::raw::c_void,
        /// (位置, 长度, AXValue 实际类型):写回时要用同一个类型创建 AXValue
        sel: Option<(isize, isize, u32)>,
    }
    // CF/AX 对象的引用计数是原子的,跨线程持有安全(记忆在主线程、恢复在粘贴线程)
    unsafe impl Send for RememberedFocus {}

    static REMEMBERED_FOCUS: Lazy<Mutex<Option<RememberedFocus>>> = Lazy::new(|| Mutex::new(None));

    /// 记住前台应用当前的焦点窗口(AXFocusedWindow)与焦点控件(AXFocusedUIElement)。
    ///
    /// 在呼出剪贴板窗口之前调用。macOS 的 ⌘V 打给前台应用的 key window,而应用
    /// 重新被激活时 key window/焦点控件恢复成什么样是**应用自己决定的**——
    /// 多窗口应用常回到主窗口,单窗口内嵌页应用干脆不恢复控件焦点,⌘V 就打不进
    /// 用户原本在打字的地方。所以显式记下,粘贴前再设回去(restore_focused_window)。
    pub fn remember_focused_window() {
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace.is_null() {
                return;
            }
            let app: *mut Object = msg_send![workspace, frontmostApplication];
            if app.is_null() {
                return;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            // 前台是自己时保留上一次的记录(与 X11 记目标窗口同一策略:窗口本来就开着)
            if pid <= 0 || pid as u32 == std::process::id() {
                return;
            }

            let ax_app = AXUIElementCreateApplication(pid);
            if ax_app.is_null() {
                return;
            }
            let mut window: *mut std::os::raw::c_void = std::ptr::null_mut();
            let mut element: *mut std::os::raw::c_void = std::ptr::null_mut();
            let werr = with_cf_attr("AXFocusedWindow", |attr| {
                AXUIElementCopyAttributeValue(ax_app, attr, &mut window)
            });
            let eerr = with_cf_attr("AXFocusedUIElement", |attr| {
                AXUIElementCopyAttributeValue(ax_app, attr, &mut element)
            });
            cf_release(ax_app);
            if werr != KAX_ERROR_SUCCESS || window.is_null() {
                cf_release(element);
                // 记不住(常见原因:目标应用没暴露 AX / 未授权)就清掉旧记录,
                // 粘贴时走系统默认行为,与修复前一致
                crate::klog!("[粘贴] AX 记录焦点窗口失败: err={werr}");
                *REMEMBERED_FOCUS.lock() = None;
                return;
            }
            if eerr != KAX_ERROR_SUCCESS || element.is_null() {
                // 窗口能记住、控件记不住(如未开启 AX 支持的 Electron 应用):
                // 只做窗口级恢复(旧版行为)
                element = std::ptr::null_mut();
            }
            // CopyAttributeValue 成功时返回的就是已持有的引用,直接存,别再 CFRetain
            let title = ax_string(window, "AXTitle").unwrap_or_default();
            let (role, sel) = if element.is_null() {
                (None, None)
            } else {
                (ax_string(element, "AXRole"), ax_selected_range(element))
            };
            crate::klog!(
                "[粘贴] 已用 AX 记住前台 pid={pid}: 窗口=「{title}」 焦点元素角色={role:?} 选区={sel:?}"
            );
            *REMEMBERED_FOCUS.lock() = Some(RememberedFocus { pid, window, element, sel });
        }
    }

    /// 粘贴前把记下的窗口/控件恢复成前台应用的 key window 与焦点控件。
    ///
    /// 应用此刻必须已是前台(调用前 paste_steps 已等激活态让出),否则设置无效。
    /// 实测部分应用(Qt/CEF)对 AX 设焦点"假成功"(返回成功但没生效),所以每次
    /// 设置后读回校验,不匹配就重试——光标丢失的场景读回会持续不匹配,日志可见。
    pub fn restore_focused_window() {
        let Some(remembered) = REMEMBERED_FOCUS.lock().take() else {
            return;
        };
        unsafe {
            let ax_app = AXUIElementCreateApplication(remembered.pid);
            if ax_app.is_null() {
                cf_release(remembered.window);
                cf_release(remembered.element);
                return;
            }
            if !remembered.window.is_null() {
                // 窗口级:AXRaise(摆到最前)+ 设 key window,不同应用只认其一,两个都做
                let raise =
                    with_cf_attr("AXRaise", |action| {
                        AXUIElementPerformAction(remembered.window, action)
                    });
                let set = with_cf_attr("AXFocusedWindow", |attr| {
                    AXUIElementSetAttributeValue(ax_app, attr, remembered.window)
                });
                crate::klog!("[粘贴] 窗口级恢复: AXRaise={raise} 设焦点={set}");
            }
            if !remembered.element.is_null() {
                // 先读回一次:应用的焦点控件若仍是它,就不要再设 AXFocused——
                // 对 web 编辑区重复设焦点会把光标重置到开头
                let already = with_cf_attr("AXFocusedUIElement", |attr| {
                    let mut back: *mut std::os::raw::c_void = std::ptr::null_mut();
                    let err = AXUIElementCopyAttributeValue(ax_app, attr, &mut back);
                    let hit = err == KAX_ERROR_SUCCESS
                        && !back.is_null()
                        && CFEqual(back, remembered.element);
                    if !back.is_null() {
                        CFRelease(back);
                    }
                    hit
                });
                if already {
                    crate::klog!("[粘贴] 控件已是 AX 焦点,跳过设置");
                }
                // 控件级:对焦点元素设 AXFocused=true,直接把光标所在控件设回焦点
                for attempt in 1..=3 {
                    if already {
                        break;
                    }
                    let set = with_cf_attr("AXFocused", |attr| {
                        AXUIElementSetAttributeValue(
                            remembered.element,
                            attr,
                            kCFBooleanTrue as *mut std::os::raw::c_void,
                        )
                    });
                    let matched = with_cf_attr("AXFocusedUIElement", |attr| {
                        let mut back: *mut std::os::raw::c_void = std::ptr::null_mut();
                        let err =
                            AXUIElementCopyAttributeValue(ax_app, attr, &mut back);
                        let hit = err == KAX_ERROR_SUCCESS
                            && !back.is_null()
                            && CFEqual(back, remembered.element);
                        if !back.is_null() {
                            CFRelease(back);
                        }
                        hit
                    });
                    crate::klog!(
                        "[粘贴] 控件级恢复(第{attempt}次): 设焦点={set} 读回匹配={matched}"
                    );
                    if matched {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(80));
                }
                // 设 AXFocused 会让应用把光标重置到文本开头:把记忆的选区写回,
                // ⌘V 才落在用户原本的位置(带选区时粘贴替换选中内容,符合直觉)
                if let Some((location, length, value_type)) = remembered.sel {
                    let set_range = with_cf_attr("AXSelectedTextRange", |attr| {
                        let range = CFRange { location, length };
                        let value = AXValueCreate(
                            value_type,
                            &range as *const CFRange as *const std::os::raw::c_void,
                        );
                        if value.is_null() {
                            return -1;
                        }
                        let result =
                            AXUIElementSetAttributeValue(remembered.element, attr, value);
                        cf_release(value);
                        result
                    });
                    crate::klog!(
                        "[粘贴] 恢复光标位置({location},{length},类型={value_type}): {set_range}"
                    );
                }
            }
            cf_release(ax_app);
            cf_release(remembered.window);
            cf_release(remembered.element);
        }
    }

    /// 显式放弃激活态,把"最前应用"交给窗口列表里的下一个应用。
    ///
    /// `app.hide()`(NSApp hide:)一般会把激活态让出去,但实测出现过"窗口隐藏了、
    /// 我们仍是最前应用"的情况,这时注入的 ⌘V 会打进自己的窗口(表现为"只有第一次能粘贴")。
    pub fn deactivate_self() {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            if !app.is_null() {
                let _: () = msg_send![app, deactivate];
            }
        }
    }

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

/// 放弃激活态:macOS 上显式 `NSApp deactivate`,把激活态交给下一个应用。
/// X11 由窗口管理器负责,不需要这个动作,所以只在 mac 上提供。
#[cfg(target_os = "macos")]
pub fn deactivate_self() {
    platform::deactivate_self();
}

/// 呼出剪贴板前记住前台应用的焦点窗口(macOS;其他平台无事可做)
#[cfg(target_os = "macos")]
pub fn remember_focused_window() {
    platform::remember_focused_window();
}

/// 粘贴前把记下的窗口设回前台应用的 key window(macOS)
#[cfg(target_os = "macos")]
pub fn restore_focused_window() {
    platform::restore_focused_window();
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    pub fn frontmost_app_name() -> Option<String> {
        None
    }
}
