//! 粘贴:写回剪贴板 + 模拟 Ctrl+V(参考 QuickClipboard paste/keyboard.rs)

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clipboard_rs::{Clipboard, ClipboardContent};
#[allow(unused_imports)] // Manager 只在非 macOS 分支里用(get_window)
use tauri::{AppHandle, Manager};

use crate::clipboard_service;
use crate::klog;
use crate::state::{self, ClipboardRecord};

/// 粘贴是一条"写剪贴板 → 隐藏窗口 → 归还焦点 → 注入 Ctrl+V"的长链路,耗时上百毫秒。
///
/// 命令必须带 `(async)`,由 Tauri 调度到线程池执行:
/// 不带 async 的同步命令跑在 GTK 主线程上,`window.hide()` 这类窗口请求只能排在
/// 命令返回之后才被处理,于是"窗口还占着焦点时 Ctrl+V 就已经发出去了",目标应用
/// 什么也收不到——麒麟上的表现就是"剪贴板窗口消失了,但没有任何内容被粘贴"。
/// 粘贴互斥:整条"写剪贴板→隐藏→归还焦点→注入"链路同一时刻只允许一条,
/// 两条并发会互相踩(焦点/修饰键状态交错,注入变成裸 v 或落空)。
/// 前端已过滤按键自动重复,这里是兜底(比如极快地连点两条记录)。
static PASTE_BUSY: AtomicBool = AtomicBool::new(false);

/// 特殊操作:纯文本粘贴。**必须 async**:粘贴链路里有最长 2s 的等待
/// (隐藏窗口/等激活态让出),同步命令会占住主线程,NSApp hide 与排队到
/// 主线程的操作都得不到处理,前台永远切不出去,⌘V 发进空档(粘贴无反应)。
#[tauri::command(async)]
pub fn cmd_paste_plain(app: AppHandle, id: u64) -> Result<(), String> {
    if PASTE_BUSY
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        klog!("[粘贴] 已有粘贴在进行,忽略纯文本粘贴请求(id={id})");
        return Ok(());
    }
    let result = match state::get_by_id(id) {
        Some(rec) => {
            let r = paste_record_plain(&app, &rec);
            if let Err(e) = &r {
                klog!("[粘贴] 纯文本粘贴命令失败: {e}");
            }
            r
        }
        None => Err("记录不存在".into()),
    };
    PASTE_BUSY.store(false, Ordering::SeqCst);
    result
}

#[tauri::command(async)]
pub fn cmd_paste_item(app: AppHandle, id: u64) -> Result<(), String> {
    if PASTE_BUSY
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        klog!("[粘贴] 已有粘贴在进行,忽略本次请求(id={id})");
        return Ok(());
    }
    let result = do_paste_item(&app, id);
    PASTE_BUSY.store(false, Ordering::SeqCst);
    result
}

fn do_paste_item(app: &AppHandle, id: u64) -> Result<(), String> {
    let rec = state::get_by_id(id).ok_or("记录不存在")?;
    let result = paste_record(app, &rec);
    // 粘贴失败时窗口已经隐藏,前端的错误横幅是看不到的,必须打到日志上
    if let Err(e) = &result {
        klog!("[粘贴] 命令失败: {e}");
    }
    result
}

pub fn paste_record(app: &AppHandle, rec: &ClipboardRecord) -> Result<(), String> {
    klog!(
        "[粘贴] 开始: 类型={} 哈希前8={}",
        rec.kind,
        &rec.hash[..rec.hash.len().min(8)]
    );

    // 粘贴会写回剪贴板,先抑制监听器,避免把自己的写入当成一条新记录
    state::suppress_monitor_for(4000);

    let outcome = paste_steps(app, rec);

    // 失败时窗口已经藏起来了,而"唤起窗口"的全局热键本身也可能没绑上
    // (麒麟上就有这种情况)。所以失败一律把窗口收回来,避免用户以为程序崩了/找不回界面。
    if let Err(e) = &outcome {
        klog!("[粘贴] 失败: {e} —— 把主窗口重新显示出来,避免界面丢失");
        crate::show_main(app);
    }
    outcome
}

/// 以纯文本粘贴:与普通粘贴**完全相同的链路**(同一函数、同一时序),
/// 只是临时清掉 html 字段,write_clipboard 自然落到纯文本分支(_ => set_text)。
/// 之前单独抄一条链路,mac 上隐藏后的时序与正常粘贴不同,⌘V 频繁落空。
pub fn paste_record_plain(app: &AppHandle, rec: &ClipboardRecord) -> Result<(), String> {
    let mut plain_rec = rec.clone();
    plain_rec.html = None;
    plain_rec.kind = "text".into();
    klog!(
        "[粘贴] 纯文本粘贴(复用普通链路): 哈希前8={}",
        &rec.hash[..rec.hash.len().min(8)]
    );
    paste_record(app, &plain_rec)
}

/// 粘贴主链路:写剪贴板 → 隐藏窗口 → 归还焦点 → 注入快捷键
fn paste_steps(app: &AppHandle, rec: &ClipboardRecord) -> Result<(), String> {
    // 1. 先写剪贴板:此时窗口还在,出错能立刻反馈给前端
    write_clipboard(app, rec)?;
    klog!("[粘贴] 剪贴板已写入");

    // 2. 隐藏窗口,把焦点让出去。
    //    置顶(钉住)时也必须短暂隐藏:目标应用要拿到键盘焦点才能接收注入的快捷键,
    //    "保持显示只归还焦点"在 macOS(应用激活态)与 UKUI(拒绝切换焦点)上都不可靠;
    //    粘贴完成后再把窗口请回来(见步骤 5),用户看到的是一次短暂闪烁。
    //    这同时避开了 UKUI 重映射丢置顶的坑(呼出路径会补发 ABOVE)。
    crate::hide_main(app);
    let hidden = wait_until_hidden(app, Duration::from_millis(800));
    klog!("[粘贴] 主窗口已隐藏: {hidden}");

    // 3. 确保焦点落回"唤起剪贴板之前的那个窗口"
    restore_focus();
    std::thread::sleep(Duration::from_millis(120));

    // macOS 特例:app.hide() 之后偶尔我们仍是"最前"的那个应用(激活态没让出去),
    // 这时注入的 ⌘V 会打进我们自己的窗口,表现就是"只有第一次能粘贴"。
    // 检测到这种情况就显式 deactivate 一次,把激活态交给下一个应用。
    // macOS:app.hide() 后激活态让出是异步的,前台常常暂时还是自己。
    // 显式 deactivate 后**轮询**等前台真正变回其它应用再注入——之前固定睡 150ms,
    // 切换慢时 ⌘V 会落进切换的空档里,粘贴无任何反应。
    #[cfg(target_os = "macos")]
    {
        let started = std::time::Instant::now();
        let handle = app.clone();
        // hide 之后系统会自动把前台交给上一个应用(实测 ~100-200ms 完成);
        // 立刻 deactivate 会取消这次自动切换,系统陷入"无激活应用"状态,
        // ⌘V 发进空档(表现:粘贴无任何反应)。先等自然让出,400ms 仍未让出才兜底。
        let mut deactivated = false;
        while crate::appinfo::active_app_name().is_none()
            && started.elapsed() < Duration::from_millis(1200)
        {
            if !deactivated && started.elapsed() >= Duration::from_millis(400) {
                klog!("[粘贴] 前台迟迟未让出,显式 deactivate 兜底");
                // AppKit 只允许在主线程调用,必须 dispatch 回主线程(命令跑在线程池里)
                let _ = handle.run_on_main_thread(|| crate::appinfo::deactivate_self());
                deactivated = true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if deactivated {
            klog!("[粘贴] 激活态已让出(等待 {}ms)", started.elapsed().as_millis());
        }
    }

    // 注入前把"现在前台是谁"打出来:如果这里是我们自己(None)或空,说明焦点没让出去,
    // 注入的快捷键会打空 —— 排查"只能粘一次"这类问题全靠这一行
    klog!(
        "[粘贴] 注入前: 前台应用={:?} 窗口可见(镜像)={}",
        crate::appinfo::active_app_name(),
        crate::window_visible(),
    );

    // 4. 注入粘贴快捷键(macOS 是 ⌘V,其余平台 Ctrl+V)
    simulate_paste()?;
    klog!("[粘贴] 粘贴快捷键已发送");

    // 5. 置顶模式:给目标应用留一点处理按键的时间,然后把窗口恢复显示。
    //    show_main 在 Linux 上会顺带补发 ABOVE,修复 UKUI 重映射丢置顶的问题。
    if crate::is_always_on_top() {
        std::thread::sleep(Duration::from_millis(250));
        crate::show_main(app);
        klog!("[粘贴] 置顶模式:窗口已恢复显示");
    }
    Ok(())
}

/// 等待窗口真正隐藏(窗口请求是投递到主循环异步处理的)。
///
/// 查询窗口可见性必须回到主线程做:Linux 上 `is_visible()` 直接读 GTK,
/// 工作线程里调用会崩(麒麟上的"点条目粘贴直接崩溃"就是这个)。
fn wait_until_hidden(app: &AppHandle, timeout: Duration) -> bool {
    // macOS 走的是 app.hide()(NSApp hide:),它同步生效、立刻把激活态交还给上一个应用,
    // 但窗口自身的 is_visible 标志不一定跟着翻转,轮询它只会白等超时,所以这里直接放行
    #[cfg(target_os = "macos")]
    {
        let _ = (app, timeout);
        return true;
    }

    #[cfg(not(target_os = "macos"))]
    {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if is_visible_via_main(app) == Some(false) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// 请主线程读一次窗口可见性并把结果传回来(拿不到就返回 None,由调用方决定怎么兜底)
#[cfg(not(target_os = "macos"))]
fn is_visible_via_main(app: &AppHandle) -> Option<bool> {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let value = handle.get_window("main").and_then(|w| w.is_visible().ok());
        let _ = tx.send(value);
    })
    .ok()?;
    rx.recv_timeout(Duration::from_millis(300)).ok().flatten()
}

/// 归还焦点。
///
/// 常规窗口管理器在窗口取消映射后会把焦点还给上一个窗口,但 UKUI 等并不保证
/// (焦点可能丢给根窗口/桌面,这时模拟出来的 Ctrl+V 就发给了空气),所以这里
/// 先给窗口管理器一点时间,再按"唤起剪贴板之前的那个窗口"显式激活一次。
/// 目标窗口已经拿到焦点时,EWMH 激活请求是幂等的,基本立即返回。
fn restore_focus() {
    #[cfg(target_os = "linux")]
    {
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        while std::time::Instant::now() < deadline && !crate::x11::focus_left_app() {
            std::thread::sleep(Duration::from_millis(25));
        }
        match crate::x11::target_window() {
            Some(win) => {
                let ok = crate::x11::activate_window(win);
                klog!("[粘贴] 归还焦点到 0x{win:x},结果={ok}");
            }
            None => klog!("[粘贴] 没有记录过目标窗口,依赖窗口管理器归还焦点"),
        }
    }
}

fn simulate_paste() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let result = crate::x11::send_ctrl_v();
    #[cfg(not(target_os = "linux"))]
    let result = simulate_paste_enigo();
    result
}

/// macOS 预览路径:用 enigo 发粘贴快捷键(需要辅助功能权限,见 HANDOVER 已知问题)。
///
/// **注意修饰键:macOS 的粘贴是 ⌘V(Command),不是 Ctrl+V**。
/// 之前这里发的是 Ctrl+V,表现就是"权限正常、事件也确实发出去了,但目标应用毫无反应"
/// (macOS 里 Ctrl+V 几乎没有应用绑定)。enigo 里 `Key::Meta` 在 macOS 上映射到 Command。
#[cfg(not(target_os = "linux"))]
fn simulate_paste_enigo() -> Result<(), String> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};

    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| match e {
        enigo::NewConError::NoPermission => {
            "无输入模拟权限:系统设置 → 隐私与安全性 → 辅助功能,授权给 KylinPaste\
             (开发模式下是启动它的终端/IDE,而且每次重编译都可能失效),授权后重启应用"
                .to_string()
        }
        other => format!("创建键盘模拟器失败: {other:?}"),
    })?;

    let modifier = if cfg!(target_os = "macos") {
        Key::Meta // macOS:Command
    } else {
        Key::Control
    };
    // macOS 上 Key::Unicode 依赖当前键盘布局查表,中文输入法环境下可能查错键码,
    // 直接用 V 的虚拟键码 9(kVK_ANSI_V)最稳
    let v_key = if cfg!(target_os = "macos") {
        Key::Other(9)
    } else {
        Key::Unicode('v')
    };

    enigo
        .key(modifier, Direction::Press)
        .map_err(|e| format!("按下修饰键失败: {e:?}"))?;
    enigo
        .key(v_key, Direction::Press)
        .map_err(|e| format!("按下 V 失败: {e:?}"))?;

    std::thread::sleep(Duration::from_millis(8));

    enigo
        .key(v_key, Direction::Release)
        .map_err(|e| format!("释放 V 失败: {e:?}"))?;
    enigo
        .key(modifier, Direction::Release)
        .map_err(|e| format!("释放修饰键失败: {e:?}"))?;

    Ok(())
}

fn write_clipboard(app: &AppHandle, rec: &ClipboardRecord) -> Result<(), String> {
    // 复用一个常驻的写上下文:每次新建都会多一条 X 连接 + 一个常驻线程
    clipboard_service::with_writer(|ctx| match rec.kind.as_str() {
        // 旧版本存储的文件条目是 file:// + 百分号编码的 URI,归一化为本地路径:
        // set_files 写出的 text/plain 才是真实路径(OA 类应用据此弹上传框)
        "files" => {
            let files: Vec<String> = rec
                .files
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|f| clipboard_service::normalize_file_entry(f))
                .collect();
            #[cfg(target_os = "linux")]
            {
                // Linux:uri-list/gnome(文件通道)+ UTF8_STRING 写 file:// URI(OA 上传触发)
                ctx.set(clipboard_service::file_clipboard_payload(&files))
                    .map_err(|e| format!("写入文件列表失败: {e}"))
            }
            #[cfg(not(target_os = "linux"))]
            {
                ctx.set_files(files).map_err(|e| format!("写入文件列表失败: {e}"))
            }
        }
        "image" => {
            let img = crate::clipboard_service::load_image(
                app,
                rec.image_path.as_deref().unwrap_or_default(),
            )?;
            // "复制图片文件"来的记录额外带着原始路径:把文件列表一起写进剪贴板,
            // 这样粘到文件管理器还是文件、粘到文档/聊天还是图片
            match rec.files.as_deref().filter(|files| !files.is_empty()) {
                Some(files) => {
                    let files: Vec<String> =
                        files.iter().map(|f| clipboard_service::normalize_file_entry(f)).collect();
                    let mut contents = vec![ClipboardContent::Image(img)];
                    #[cfg(target_os = "linux")]
                    contents.extend(clipboard_service::file_clipboard_payload(&files));
                    #[cfg(not(target_os = "linux"))]
                    contents.push(ClipboardContent::Files(files));
                    ctx.set(contents).map_err(|e| format!("写入图片与文件失败: {e}"))
                }
                None => ctx.set_image(img).map_err(|e| format!("写入图片失败: {e}")),
            }
        }
        // 富文本:html 和纯文本**一起**写回,目标应用各取所需。
        // 只写 html(set_html)的话,纯文本目标(终端、记事本类、部分 Linux 程序)
        // 在剪贴板上取不到 text/plain,表现就是"粘不出来",像是不支持富文本。
        "html" => match rec
            .text
            .as_deref()
            .filter(|plain| !plain.trim().is_empty())
        {
            Some(plain) => ctx
                .set(vec![
                    ClipboardContent::Html(rec.html.clone().unwrap_or_default()),
                    ClipboardContent::Text(plain.to_string()),
                ])
                .map_err(|e| format!("写入富文本失败: {e}")),
            None => ctx
                .set_html(rec.html.clone().unwrap_or_default())
                .map_err(|e| format!("写入富文本失败: {e}")),
        },
        _ => ctx
            .set_text(rec.text.clone().unwrap_or_default())
            .map_err(|e| format!("写入文本失败: {e}")),
    })
}
