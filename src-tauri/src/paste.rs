//! 粘贴:写回剪贴板 + 模拟 Ctrl+V(参考 QuickClipboard paste/keyboard.rs)

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use clipboard_rs::{Clipboard, ClipboardContent};
use tauri::{AppHandle, Manager}; // Manager:非 macOS 的 get_window + 各平台的 emit_all

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
///
/// 存的是"这次粘贴开始的时刻"而不是布尔:万一条链路 panic(或者以后有人在复位
/// 之前插了一句提前返回),布尔会永久卡在"忙",之后所有粘贴都被静默忽略——
/// 用户只看到"点了没反应"。带超时后即使漏了复位也能自愈。
static PASTE_STARTED_MS: AtomicU64 = AtomicU64::new(0);
/// 超过这么久还没结束,就认为上一次粘贴已经异常死掉,放新的进来。
/// (正常链路最长约 2.4s:隐藏窗口 800ms + mac 等激活态让出 1200ms + 两次等待)
const PASTE_BUSY_TIMEOUT_MS: u64 = 15_000;

/// 粘贴通道的占用句柄:被释放时(包含提前 `return` 与 panic 展开)自动解锁,
/// 不会因为漏写一句复位就把后续粘贴全堵死。
struct PasteGuard {
    started_ms: u64,
}

impl PasteGuard {
    /// 尝试占用粘贴通道;返回 `None` 表示已有一条粘贴在进行
    fn acquire() -> Option<Self> {
        let now = state::now_ms();
        loop {
            let last = PASTE_STARTED_MS.load(Ordering::SeqCst);
            if last != 0 && now.saturating_sub(last) < PASTE_BUSY_TIMEOUT_MS {
                return None;
            }
            // 两个线程同时进来时,只有一个能把"空闲/已超时"改写成自己的时刻
            if PASTE_STARTED_MS
                .compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Some(Self { started_ms: now });
            }
        }
    }
}

impl Drop for PasteGuard {
    fn drop(&mut self) {
        // 只清自己那一次占用:万一自己早就超时、别的粘贴已经接手,不能把对方的占用清掉
        let _ = PASTE_STARTED_MS.compare_exchange(
            self.started_ms,
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

/// 特殊操作:纯文本粘贴。**必须 async**:粘贴链路里有最长 2s 的等待
/// (隐藏窗口/等激活态让出),同步命令会占住主线程,NSApp hide 与排队到
/// 主线程的操作都得不到处理,前台永远切不出去,⌘V 发进空档(粘贴无反应)。
#[tauri::command(async)]
pub fn cmd_paste_plain(app: AppHandle, id: u64) -> Result<(), String> {
    let Some(_guard) = PasteGuard::acquire() else {
        klog!("[粘贴] 已有粘贴在进行,忽略纯文本粘贴请求(id={id})");
        return Ok(());
    };
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
    // 纯文本粘贴同样是"用了这条记录",一样置顶(与普通粘贴保持一致)
    if result.is_ok() {
        promote_after_paste(&app, id);
    }
    result
}

#[tauri::command(async)]
pub fn cmd_paste_item(app: AppHandle, id: u64) -> Result<(), String> {
    let Some(_guard) = PasteGuard::acquire() else {
        klog!("[粘贴] 已有粘贴在进行,忽略本次请求(id={id})");
        return Ok(());
    };
    let result = do_paste_item(&app, id);
    if result.is_ok() {
        promote_after_paste(&app, id);
    }
    result
}

/// 粘贴成功后把这条记录挪到列表最前、刷新时间戳(不产生新记录),再通知前端重排。
///
/// 只在**成功**时做:失败时窗口会被重新显示出来,列表不该发生位移。
/// 前端收到 `record-promoted` 后按同样的规则重排(移动 + 用新时间戳,不新增条目)。
fn promote_after_paste(app: &AppHandle, id: u64) {
    match state::promote(id) {
        Some(rec) => {
            klog!("[粘贴] 已把 id={id} 挪到列表最前并刷新时间戳");
            let _ = app.emit_all("record-promoted", state::RecordView::from(&rec));
        }
        // 极少数情况:粘贴链路走完时这条已经被淘汰/删除了
        None => klog!("[粘贴] 想在粘贴后置顶 id={id},但这条已不在历史里"),
    }
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

    // 2. (仅 Linux)趁自己还是活动窗口,先真切换激活目标窗口。
    //    此时 WM 的活动窗口是我们,EWMH 激活目标必然是真实切换,WM 会走完整
    //    激活协议(给我们 FocusOut,给目标 FocusIn + WM_TAKE_FOCUS),应用据此
    //    恢复内部控件焦点——等价于 xdotool windowactivate 实验的机制。
    //    之后再隐藏自己:那时我们已不是活动窗口,取消映射不会触发焦点回收,
    //    也就不会出现"UKUI 自动还焦点导致后续激活变空操作"的问题。
    //    置顶(钉住)时也必须隐藏:目标应用要拿到键盘焦点才能接收注入的快捷键。
    #[cfg(target_os = "linux")]
    let pre_activated = match crate::x11::target_window() {
        Some(win) => {
            let ok = crate::x11::activate_window(win);
            klog!("[粘贴] 隐藏前先真切换激活目标窗口: 0x{win:x},结果={ok}");
            ok
        }
        None => false,
    };
    #[cfg(not(target_os = "linux"))]
    let pre_activated = false;

    // 3. 隐藏窗口,把焦点让出去。
    //    置顶(钉住)时也必须短暂隐藏:目标应用要拿到键盘焦点才能接收注入的快捷键;
    //    粘贴完成后再把窗口请回来(见步骤 5),用户看到的是一次短暂闪烁。
    //    这同时避开了 UKUI 重映射丢置顶的坑(呼出路径会补发 ABOVE)。
    //    Linux 上此时目标窗口已被前置激活(步骤 2),自己是普通窗口,取消映射
    //    不会再引发焦点回收。
    crate::hide_main(app);
    let hidden = wait_until_hidden(app, Duration::from_millis(800));
    klog!("[粘贴] 主窗口已隐藏: {hidden}");

    // 4. 确保焦点落回"唤起剪贴板之前的那个窗口"
    restore_focus(pre_activated);
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

    // macOS:应用重新拿到激活态后,key window 是应用自己挑的——多窗口应用
    // (如邮件客户端的写信窗口)常常回到主窗口,而不是用户原来在打字的窗口。
    // 把呼出前用 AX 记下的焦点窗口设回去,⌘V 才会落到正确的窗口。
    #[cfg(target_os = "macos")]
    crate::appinfo::restore_focused_window();

    // 5. 注入粘贴快捷键(macOS 是 ⌘V,其余平台 Ctrl+V)
    simulate_paste()?;
    klog!("[粘贴] 粘贴快捷键已发送");
    // 注入后焦点快照:确认按键真的送进了目标窗口(排查"没粘贴进来"类问题的关键证据)
    #[cfg(target_os = "linux")]
    klog!("[粘贴] 注入后: {}", crate::x11::focus_snapshot());

    // 6. 置顶模式:给目标应用留一点处理按键的时间,然后把窗口恢复显示。
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
/// Linux 上常规路径已在隐藏前用"真切换"前置激活了目标窗口(paste_steps 步骤 2),
/// 这里只做兜底:前置激活没成功时补一次激活——目标还不是活动窗口的话,借桌面
/// 制造真切换;已是活动窗口(UKUI 自动还焦点)则只补发 WM_TAKE_FOCUS。
/// 无论哪种,最后都补发一次带真实时间戳的 WM_TAKE_FOCUS(幂等,正常应用无感)。
fn restore_focus(pre_activated: bool) {
    #[cfg(target_os = "linux")]
    {
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        while std::time::Instant::now() < deadline && !crate::x11::focus_left_app() {
            std::thread::sleep(Duration::from_millis(25));
        }
        match crate::x11::target_window() {
            Some(win) => {
                if !pre_activated && crate::x11::target_window_is_active() {
                    // 前置激活没成功,而 UKUI 已自动把焦点还给目标窗口:
                    // 借桌面制造真切换(即发即走,60ms 后切回)
                    match crate::x11::desktop_window() {
                        Some(desktop) => {
                            let d = crate::x11::activate_window_fast(desktop);
                            klog!("[粘贴] 前置激活未成功,先激活桌面制造真切换: {d}");
                            std::thread::sleep(std::time::Duration::from_millis(60));
                        }
                        None => {
                            let unset = crate::x11::unsettle_focus();
                            klog!("[粘贴] 没有桌面窗口,退回拨焦点到根窗口: {unset}");
                        }
                    }
                }
                let ok = crate::x11::activate_window(win);
                klog!("[粘贴] 归还焦点到 0x{win:x},结果={ok}");
                let tf = crate::x11::send_wm_take_focus(win);
                klog!("[粘贴] 补发 WM_TAKE_FOCUS: {tf}");
            }
            None => klog!("[粘贴] 没有记录过目标窗口,依赖窗口管理器归还焦点"),
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pre_activated;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 粘贴互斥:占用期间拒绝新的,释放后能再进来;
    /// 而且**即使上一次异常中断没复位,超时后也能自愈**(以前是布尔,会永久卡住)
    #[test]
    fn paste_guard_rejects_concurrent_then_recovers() {
        PASTE_STARTED_MS.store(0, Ordering::SeqCst); // 与其它用例隔离

        let guard = PasteGuard::acquire().expect("空闲时应当能占用");
        assert!(PasteGuard::acquire().is_none(), "正在粘贴时应当拒绝");

        drop(guard); // 句柄一释放就该解锁,不用手写复位
        let again = PasteGuard::acquire().expect("释放后应当能再占用");
        drop(again);
        assert_eq!(
            PASTE_STARTED_MS.load(Ordering::SeqCst),
            0,
            "句柄释放后占用标记应当被清掉"
        );

        // 超时自愈:模拟"上次粘贴死在半路",时间戳停在很久以前
        PASTE_STARTED_MS.store(
            crate::state::now_ms().saturating_sub(PASTE_BUSY_TIMEOUT_MS + 1),
            Ordering::SeqCst,
        );
        let recovered = PasteGuard::acquire();
        assert!(recovered.is_some(), "超过超时阈值后应当放行新的粘贴");
        drop(recovered);
        PASTE_STARTED_MS.store(0, Ordering::SeqCst);
    }
}
