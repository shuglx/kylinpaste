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

/// 特殊操作:粘贴。`plain=true` 只写纯文本,`plain=false` 写回富文本(html+纯文本)。
///
/// 两条通道都**显式指定**格式,不受"优先粘贴纯文本"开关影响——开关只改变常规粘贴
/// 的默认行为,条目上的按钮始终按其当前语义执行。
/// **必须 async**:粘贴链路里有最长 2s 的等待(隐藏窗口/等激活态让出),同步命令会
/// 占住主线程,NSApp hide 与排队到主线程的操作都得不到处理,前台永远切不出去,
/// ⌘V 发进空档(粘贴无反应)。
fn paste_explicit(app: &AppHandle, id: u64, plain: bool) -> Result<(), String> {
    let Some(_guard) = PasteGuard::acquire() else {
        klog!("[粘贴] 已有粘贴在进行,忽略本次请求(id={id}, plain={plain})");
        return Ok(());
    };
    let result = match state::get_by_id(id) {
        Some(rec) => {
            let r = if plain {
                paste_record_plain(app, &rec)
            } else {
                paste_record(app, &rec)
            };
            if let Err(e) = &r {
                klog!("[粘贴] 特殊操作粘贴失败(plain={plain}): {e}");
            }
            r
        }
        None => Err("记录不存在".into()),
    };
    // 特殊操作粘贴同样是"用了这条记录",一样置顶(与普通粘贴保持一致)
    if result.is_ok() {
        promote_after_paste(app, id);
    }
    result
}

/// 特殊操作:粘贴纯文本(不保留格式)
#[tauri::command(async)]
pub fn cmd_paste_plain(app: AppHandle, id: u64) -> Result<(), String> {
    paste_explicit(&app, id, true)
}

/// 特殊操作:粘贴富文本(保留格式)。"优先粘贴纯文本"开启时,条目按钮由它接管。
#[tauri::command(async)]
pub fn cmd_paste_rich(app: AppHandle, id: u64) -> Result<(), String> {
    paste_explicit(&app, id, false)
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
    // "优先粘贴纯文本"开启时,文字/富文本记录的常规粘贴(点击/回车/数字键)改写纯文本;
    // 其余类型(图片/文件)不受影响。条目上的特殊操作按钮走 paste_explicit,不会被这里改写。
    let result = if prefers_plain(rec.kind.as_str()) {
        klog!("[粘贴] 已开启优先粘贴纯文本,本条按纯文本粘贴");
        paste_record_plain(app, &rec)
    } else {
        paste_record(app, &rec)
    };
    // 粘贴失败时窗口已经隐藏,前端的错误横幅是看不到的,必须打到日志上
    if let Err(e) = &result {
        klog!("[粘贴] 命令失败: {e}");
    }
    result
}

/// 是否按"优先粘贴纯文本"处理:开关开着,且这条记录是文字/富文本类型。
fn prefers_plain(kind: &str) -> bool {
    crate::settings::get().prefer_plain_text && matches!(kind, "text" | "html")
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

/// 粘贴主链路。
///
/// Linux:写剪贴板 → 真切换激活目标窗口 → 注入 → 隐藏自己。
/// 激活必须在隐藏之前做:此时自己还是活动窗口,EWMH 激活目标必然是"真切换",
/// WM 会走完整激活协议(FocusIn + WM_TAKE_FOCUS),Qt/CEF 类应用据此恢复内部
/// 控件焦点;若先隐藏自己,UKUI 会自动把活动窗口还给目标,后续激活就成空操作
/// (企业邮箱 cmclient 实测:表现为 Ctrl+V 落空/粘错位置)。而注入之后再隐藏
/// 自己——那时早已不是活动窗口,取消映射不影响焦点,只做视觉清理。
///
/// macOS:写剪贴板 → 隐藏窗口(让出激活态) → 等前台切回 → AX 恢复焦点窗口 →
/// 注入 → 置顶模式恢复显示。
fn paste_steps(app: &AppHandle, rec: &ClipboardRecord) -> Result<(), String> {
    // 1. 先写剪贴板:此时窗口还在,出错能立刻反馈给前端
    write_clipboard(app, rec)?;
    klog!("[粘贴] 剪贴板已写入");

    #[cfg(target_os = "linux")]
    {
        // 2. 趁自己还是活动窗口,真切换激活目标窗口。
        //    WM 走完整激活协议(给我们 FocusOut,给目标 FocusIn + WM_TAKE_FOCUS),
        //    应用据此恢复内部控件焦点——等价于 xdotool windowactivate 实验的机制。
        match crate::x11::target_window() {
            Some(win) => {
                // 激活前先把指针归位到呼出时的位置(用户光标所在字段):
                // cmclient 这类应用激活时把焦点路由到"指针下方的控件",
                // 用户点选面板条目会把指针带进面板,归位才能命中正确字段
                let warped = crate::x11::restore_pointer_position();
                klog!("[粘贴] 指针归位到呼出时位置: {warped}");
                let ok = crate::x11::activate_window(win);
                klog!("[粘贴] 真切换激活目标窗口: 0x{win:x},结果={ok}");
            }
            None => klog!("[粘贴] 没有记录过目标窗口,依赖 WM 归还焦点"),
        }
        // 3. 给 WM/应用一点处理激活的时间(xdotool 对照实验:激活后立即注入即可)
        std::thread::sleep(Duration::from_millis(120));

        // 4. 注入:自己已不是活动窗口(被目标窗口盖住),按键直达目标应用。
        //    注意此时自己还【可见】,隐藏挪到注入之后——取消映射若发生在注入前,
        //    WM 的焦点回收会把应用刚恢复的内部焦点再次打断(cmclient 实测:
        //    收件人粘贴被"贴到正文旧光标处")
        klog!(
            "[粘贴] 注入前: 前台应用={:?} 窗口可见(镜像)={}",
            crate::appinfo::active_app_name(),
            crate::window_visible(),
        );
        simulate_paste()?;
        klog!("[粘贴] 粘贴快捷键已发送");
        klog!("[粘贴] 注入后: {}", crate::x11::focus_snapshot());

        // 5. 注入完成后隐藏自己:此时自己早已不是活动窗口,取消映射不影响焦点,
        //    只是把面板从目标窗口底下收走(也退出 alt-tab 列表)
        crate::hide_main(app);

        // 6. 置顶模式:把窗口恢复显示(show_main 顺带补发 ABOVE,修 UKUI 丢置顶)
        if crate::is_always_on_top() {
            std::thread::sleep(Duration::from_millis(250));
            crate::show_main(app);
            klog!("[粘贴] 置顶模式:窗口已恢复显示");
        }
        return Ok(());
    }

    #[cfg(not(target_os = "linux"))]
    {
        // 2. 隐藏窗口,把焦点让出去。置顶(钉住)时也必须短暂隐藏:目标应用要拿到
        //    键盘焦点才能接收注入的快捷键;粘贴完成后再把窗口请回来(见步骤 5)。
        //    这同时避开了 UKUI 重映射丢置顶的坑(呼出路径会补发 ABOVE)。
        crate::hide_main(app);
        let hidden = wait_until_hidden(app, Duration::from_millis(800));
        klog!("[粘贴] 主窗口已隐藏: {hidden}");

        // 3. 等前台真正切回其它应用,再恢复 AX 记下的焦点窗口。
        //    app.hide() 后激活态让出是异步的:先等自然让出,400ms 仍未让出才兜底
        //    显式 deactivate——立刻 deactivate 会取消自动切换,⌘V 落进空档。
        let started = std::time::Instant::now();
        let handle = app.clone();
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
        klog!(
            "[粘贴] 注入前: 前台应用={:?} 窗口可见(镜像)={}",
            crate::appinfo::active_app_name(),
            crate::window_visible(),
        );
        // 应用重新拿到激活态后,key window 是应用自己挑的——多窗口应用(如邮件
        // 客户端的写信窗口)常常回到主窗口而不是用户原来在打字的窗口,需要把
        // 呼出前用 AX 记下的焦点窗口/控件设回去,⌘V 才会落到正确的位置
        crate::appinfo::restore_focused_window();

        // 4. 注入粘贴快捷键(⌘V)
        simulate_paste()?;
        klog!("[粘贴] 粘贴快捷键已发送");

        // 5. 置顶模式:给目标应用留一点处理按键的时间,然后把窗口恢复显示
        if crate::is_always_on_top() {
            std::thread::sleep(Duration::from_millis(250));
            crate::show_main(app);
            klog!("[粘贴] 置顶模式:窗口已恢复显示");
        }
        Ok(())
    }
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
