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
    use std::time::Instant;

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
        fn CFRetain(cf: *mut std::os::raw::c_void);
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
        fn CFArrayGetCount(array: *const std::os::raw::c_void) -> isize;
        fn CFArrayGetValueAtIndex(
            array: *const std::os::raw::c_void,
            index: isize,
        ) -> *const std::os::raw::c_void;
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

    /// 读应用的焦点控件,返回 (元素, 选区, 角色)。
    ///
    /// AX 树懒加载刚建好时,焦点元素会分阶段演进(实测:窗口 → AXWebArea →
    /// AXTextArea),所以跳过 AXWindow 继续重试,直到读到具体控件或超时。
    /// 首轮先给应用设 AXManualAccessibility=true:CEF/Electron 系应用要客户端
    /// 主动声明才会开启渲染进程的可达性支持,否则 -25212 会一直读不到。
    unsafe fn read_focused_element(
        pid: i32,
        tries: usize,
        interval: std::time::Duration,
    ) -> Option<(*mut std::os::raw::c_void, Option<(isize, isize, u32)>, Option<String>)> {
        let mut read_fail = 0usize;
        let mut window_seen = 0usize;
        for attempt in 0..tries {
            let ax_app = AXUIElementCreateApplication(pid);
            if ax_app.is_null() {
                return None;
            }
            if attempt == 0 {
                // 声明"我们是可达性客户端":Electron/CEF 据此打开渲染进程的 AX 支持
                with_cf_attr("AXManualAccessibility", |attr| {
                    AXUIElementSetAttributeValue(
                        ax_app,
                        attr,
                        kCFBooleanTrue as *mut std::os::raw::c_void,
                    )
                });
            }
            let mut el: *mut std::os::raw::c_void = std::ptr::null_mut();
            let err = with_cf_attr("AXFocusedUIElement", |attr| {
                AXUIElementCopyAttributeValue(ax_app, attr, &mut el)
            });
            cf_release(ax_app);
            if err != KAX_ERROR_SUCCESS || el.is_null() {
                read_fail += 1;
                std::thread::sleep(interval);
                continue;
            }
            let role = ax_string(el, "AXRole");
            if role.as_deref() == Some("AXWindow") {
                window_seen += 1;
                cf_release(el);
                std::thread::sleep(interval);
                continue;
            }
            let sel = ax_selected_range(el);
            return Some((el, sel, role));
        }
        crate::klog!(
            "[粘贴] 焦点控件重试{tries}次无果: 读失败{read_fail}次,焦点停在窗口级{window_seen}次"
        );
        None
    }

    /// 在窗口子树里找正文编辑区(AXTextArea),返回 (元素[已持有引用], 选区)。
    ///
    /// 为什么遍历:树刚建好时 AXFocusedUIElement 还停留在窗口级,web 区焦点没同步
    /// (纯等待无效——树建好之前用户定位光标的动作,树里没有对应事件可同步);
    /// 但编辑框控件本身已在树上,内部选区(光标位置)也还保留着。
    /// 正文是 AXTextArea,收件人/主题是 AXTextField,不会混淆;多个 AXTextArea 时
    /// 优先取"选区位置非 0"的(用户在里面定位过光标),否则取第一个。
    unsafe fn find_text_area(
        element: AXUIElementRef,
        depth: usize,
        budget: &mut usize,
        deadline: Instant,
    ) -> Option<(*mut std::os::raw::c_void, Option<(isize, isize, u32)>)> {
        if depth == 0 || *budget == 0 || Instant::now() >= deadline {
            return None;
        }
        *budget -= 1;
        if ax_string(element, "AXRole").as_deref() == Some("AXTextArea") {
            // 返回的引用要跨出本层使用,Copy 拿到的是借用, CFRetain 转成持有
            CFRetain(element);
            let sel = ax_selected_range(element);
            return Some((element, sel));
        }
        let children = with_cf_attr("AXChildren", |attr| {
            let mut value: *mut std::os::raw::c_void = std::ptr::null_mut();
            if AXUIElementCopyAttributeValue(element, attr, &mut value) != KAX_ERROR_SUCCESS
                || value.is_null()
            {
                None
            } else {
                Some(value)
            }
        });
        let Some(array) = children else {
            return None;
        };
        let count = CFArrayGetCount(array);
        let mut fallback: Option<(*mut std::os::raw::c_void, Option<(isize, isize, u32)>)> = None;
        for index in 0..count {
            if Instant::now() >= deadline {
                break;
            }
            let child = CFArrayGetValueAtIndex(array, index) as AXUIElementRef;
            if let Some(found) = find_text_area(child, depth - 1, budget, deadline) {
                let pick_now = found
                    .1
                    .map(|(location, _, _)| location > 0)
                    .unwrap_or(false);
                match &fallback {
                    // 已有候选且选区非 0,直接用它,不再往后找
                    Some((_, Some((location, _, _)))) if *location > 0 => {
                        cf_release(found.0);
                        continue;
                    }
                    // 新候选选区非 0(或还没候选):换新,旧候选释放
                    _ => {
                        if let Some((old, _)) = fallback.take() {
                            cf_release(old);
                        }
                        fallback = Some(found);
                    }
                }
                if pick_now {
                    break;
                }
            }
            if *budget == 0 {
                break;
            }
        }
        cf_release(array);
        fallback
    }

    /// 读元素的屏幕位置与尺寸 (x, y, w, h)。AXValue 实际类型自适应,
    /// point/size 的负载是两个 f64(CGDouble);类型不符时解析结果是垃圾值,
    /// 靠调用方的合理性检查兜住。
    unsafe fn ax_bounds(element: AXUIElementRef) -> Option<(f64, f64, f64, f64)> {
        let read_pair = |name: &str| -> Option<[f64; 2]> {
            with_cf_attr(name, |attr| {
                let mut value: *mut std::os::raw::c_void = std::ptr::null_mut();
                if AXUIElementCopyAttributeValue(element, attr, &mut value)
                    != KAX_ERROR_SUCCESS
                    || value.is_null()
                {
                    return None;
                }
                let actual_type = AXValueGetType(value);
                let mut pair = [0f64; 2];
                let ok = AXValueGetValue(
                    value,
                    actual_type,
                    pair.as_mut_ptr() as *mut std::os::raw::c_void,
                );
                cf_release(value);
                ok.then_some(pair)
            })
        };
        let [x, y] = read_pair("AXPosition")?;
        let [w, h] = read_pair("AXSize")?;
        (w > 30.0 && h > 30.0 && x > -10_000.0 && y > -10_000.0).then_some((x, y, w, h))
    }

    /// 在屏幕坐标处模拟一次真实左键点击。
    ///
    /// 用途:应用 AX 焦点信息不可用时(CEF 树未同步出编辑区),点击是唯一被实测
    /// 证明能让应用自己恢复编辑焦点的手段(等价于用户手动点进去)。
    fn click_at(x: i32, y: i32) -> Result<(), String> {
        use enigo::{Button, Coordinate, Direction, Enigo, Mouse, Settings};
        let mut enigo =
            Enigo::new(&Settings::default()).map_err(|e| format!("创建鼠标模拟器失败: {e:?}"))?;
        enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| format!("移动鼠标失败: {e:?}"))?;
        std::thread::sleep(std::time::Duration::from_millis(30));
        enigo
            .button(Button::Left, Direction::Click)
            .map_err(|e| format!("模拟点击失败: {e:?}"))
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
                // 窗口能记住、控件记不住:CEF 类应用 AX 树懒加载,首次查询时树还没
                // 建好(表现为"新邮件窗口打开后第一次粘贴无反应")。但这次查询已经
                // 触发树开始构建,后台线程重试很快就能读到——此时我们窗口只是前置、
                // 目标应用内部焦点未动,读到的是用户当时的正文输入框(含光标位置)。
                // 等到粘贴时才补读就晚了:呼出/隐藏循环已把焦点打散,AX 报告的焦点
                // 元素会"降级"成窗口,选区信息随之丢失(实测日志)。
                element = std::ptr::null_mut();
                crate::klog!("[粘贴] 读焦点控件失败(err={eerr}),后台重试补记");
                let retry_pid = pid;
                std::thread::spawn(move || {
                    // 渲染进程可达性就绪 + 焦点状态序列化进树可能要好几秒,给足 8 秒
                    let Some((el, sel, role)) =
                        read_focused_element(retry_pid, 80, std::time::Duration::from_millis(100))
                    else {
                        crate::klog!("[粘贴] AX 焦点控件后台补记失败: 8 秒内未读到具体控件");
                        return;
                    };
                    {
                        let mut guard = REMEMBERED_FOCUS.lock();
                        let Some(remembered) = guard.as_mut() else {
                            cf_release(el); // 粘贴已把记录取走,用不上了
                            return;
                        };
                        // 只升级"没读到控件"的那条记录;控件已读到/已换新记录都不动
                        if remembered.pid != retry_pid || !remembered.element.is_null() {
                            cf_release(el);
                            return;
                        }
                        remembered.element = el;
                        remembered.sel = sel;
                    }
                    crate::klog!("[粘贴] AX 焦点控件后台补记成功: 角色={role:?} 选区={sel:?}");
                });
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

    /// 显式"用户级"激活目标应用(NSRunningApplication activateWithOptions)。
    ///
    /// 我们隐藏窗口后系统把前台交回上一个应用,但这属于被动激活:实测网易邮箱
    /// 大师这类应用此时不会把 key window/内部焦点恢复到位(窗口在前台、光标不
    /// 闪)。activateWithOptions 等价于用户点了应用,是唯一与应用自身实现无关、
    /// 走原生恢复路径的手段。
    pub fn activate_app(pid: i32) -> bool {
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace.is_null() {
                return false;
            }
            let running: *mut Object = msg_send![workspace, runningApplications];
            if running.is_null() {
                return false;
            }
            let count: usize = msg_send![running, count];
            for index in 0..count {
                let app: *mut Object = msg_send![running, objectAtIndex: index];
                if app.is_null() {
                    continue;
                }
                let app_pid: i32 = msg_send![app, processIdentifier];
                if app_pid != pid {
                    continue;
                }
                // NSApplicationActivateIgnoringOtherApps(1) | ActivateAllWindows(2)
                let _: () = msg_send![app, activateWithOptions: 3usize];
                return true;
            }
            false
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
            // 显式用户级激活:被动激活(系统自动交回前台)不恢复 key window,
            // 这里补一次等价于 alt-tab 的激活,给应用机会自己恢复内部焦点
            let activated = activate_app(remembered.pid);
            std::thread::sleep(std::time::Duration::from_millis(120));
            crate::klog!("[粘贴] 显式激活目标应用: {activated}");

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
            // 呼出时可能读不到焦点控件:CEF 类应用的 AX 树是懒加载的,首个客户端
            // 首次查询时树还没建好(实测表现为"新邮件窗口打开后第一次粘贴无反应")。
            // 到粘贴时已过去数秒,树早就建好了,补读一次;应用内部焦点一直没动,
            // 补读到的就是用户当时的正文输入框,选区也是原位置。
            let mut element = remembered.element;
            let mut sel = remembered.sel;
            // 补读到的控件没有"应用已自己恢复焦点"的先验,必须执行一次 AXFocused
            // 设置(首次粘贴失败的教训:AX 报告的焦点控件是对的,真实 first responder
            // 却没跟上,缺这一步 ⌘V 就没反应);光标被重置由下面的选区写回兜底
            let mut force_set = false;
            if element.is_null() {
                // 后台补记失败(树同步慢/不同步)时的兜底:遍历焦点窗口子树找
                // 正文编辑区——控件与选区都在树上,只是"焦点元素"指针没同步
                if !remembered.window.is_null() {
                    let mut budget = 400usize;
                    let walk_started = Instant::now();
                    let walk = find_text_area(
                        remembered.window,
                        24,
                        &mut budget,
                        walk_started + std::time::Duration::from_millis(500),
                    );
                    crate::klog!(
                        "[粘贴] 子树遍历结束: 找到={} 节点={} 耗时={}ms",
                        walk.is_some(),
                        400 - budget,
                        walk_started.elapsed().as_millis()
                    );
                    if let Some((found, found_sel)) = walk {
                        element = found;
                        sel = found_sel;
                        force_set = true;
                    }
                }
                if element.is_null() {
                    // 树遍历也没找到(如 Electron 未暴露树):读焦点控件再试一轮,
                    // 给渲染进程 1.2 秒准备时间(仅首粘贴的失败路径才走,不拖慢常规粘贴)
                    if let Some((fresh, fresh_sel, role)) =
                        read_focused_element(remembered.pid, 15, std::time::Duration::from_millis(80))
                    {
                        if role.as_deref() == Some("AXWebArea") {
                            // 树只建到 web 容器、编辑区还没序列化进来:AX 设焦点无从谈起。
                            // 真实点击是实测唯一能让应用自己恢复编辑焦点的手段
                            // (等价于用户手动点进去)——点容器中心,即正文编辑区。
                            // 代价:光标落在点击处而非原位置(原位置此刻无处可查)。
                            match ax_bounds(fresh) {
                                Some((x, y, w, h)) => {
                                    let cx = (x + w / 2.0) as i32;
                                    let cy = (y + h / 2.0) as i32;
                                    match click_at(cx, cy) {
                                        Ok(()) => {
                                            crate::klog!(
                                                "[粘贴] 焦点仅到 web 容器,已点击其中心 ({cx},{cy}) 恢复编辑焦点"
                                            );
                                            std::thread::sleep(std::time::Duration::from_millis(150));
                                        }
                                        Err(e) => {
                                            crate::klog!("[粘贴] 点击 web 容器中心失败: {e}")
                                        }
                                    }
                                }
                                None => crate::klog!("[粘贴] 读不到 web 容器 bounds,跳过点击"),
                            }
                        } else {
                            element = fresh;
                            sel = fresh_sel;
                            force_set = true;
                            crate::klog!(
                                "[粘贴] 呼出时焦点控件未就绪,粘贴时补读成功: 角色={role:?} 选区={sel:?}"
                            );
                        }
                    } else {
                        crate::klog!("[粘贴] 补读焦点控件失败,仅窗口级恢复");
                    }
                }
            }
            if !element.is_null() {
                // 先读回一次:应用的焦点控件若仍是它,就不要再设 AXFocused——
                // 对 web 编辑区重复设焦点会把光标重置到开头
                let already = !force_set
                    && with_cf_attr("AXFocusedUIElement", |attr| {
                    let mut back: *mut std::os::raw::c_void = std::ptr::null_mut();
                    let err = AXUIElementCopyAttributeValue(ax_app, attr, &mut back);
                    let hit = err == KAX_ERROR_SUCCESS
                        && !back.is_null()
                        && CFEqual(back, element);
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
                            element,
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
                            && CFEqual(back, element);
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
                // 设 AXFocused 会让应用把光标重置到文本开头:把选区写回,
                // ⌘V 才落在用户原本的位置(带选区时粘贴替换选中内容,符合直觉)
                if let Some((location, length, value_type)) = sel {
                    let set_range = with_cf_attr("AXSelectedTextRange", |attr| {
                        let range = CFRange { location, length };
                        let value = AXValueCreate(
                            value_type,
                            &range as *const CFRange as *const std::os::raw::c_void,
                        );
                        if value.is_null() {
                            return -1;
                        }
                        let result = AXUIElementSetAttributeValue(element, attr, value);
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
            cf_release(element);
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
