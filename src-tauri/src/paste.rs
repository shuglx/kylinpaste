//! 粘贴:写回剪贴板 + 模拟 Ctrl+V(参考 QuickClipboard paste/keyboard.rs)

use std::time::{Duration, Instant};

use clipboard_rs::{Clipboard, ClipboardContent};
use tauri::{AppHandle, Manager};

use crate::clipboard_service;
use crate::state::{self, ClipboardRecord};

/// 粘贴是一条"写剪贴板 → 隐藏窗口 → 归还焦点 → 注入 Ctrl+V"的长链路,耗时上百毫秒。
///
/// 命令必须带 `(async)`,由 Tauri 调度到线程池执行:
/// 不带 async 的同步命令跑在 GTK 主线程上,`window.hide()` 这类窗口请求只能排在
/// 命令返回之后才被处理,于是"窗口还占着焦点时 Ctrl+V 就已经发出去了",目标应用
/// 什么也收不到——麒麟上的表现就是"剪贴板窗口消失了,但没有任何内容被粘贴"。
#[tauri::command(async)]
pub fn cmd_paste_item(app: AppHandle, id: u64) -> Result<(), String> {
    let rec = state::get_by_id(id).ok_or("记录不存在")?;
    let result = paste_record(&app, &rec);
    // 粘贴失败时窗口已经隐藏,前端的错误横幅是看不到的,必须打到 stderr 上
    if let Err(e) = &result {
        eprintln!("[粘贴] 失败: {e}");
    }
    result
}

pub fn paste_record(app: &AppHandle, rec: &ClipboardRecord) -> Result<(), String> {
    println!(
        "[粘贴] 开始: 类型={} 哈希前8={}",
        rec.kind,
        &rec.hash[..rec.hash.len().min(8)]
    );

    // 粘贴会写回剪贴板,先抑制监听器,避免把自己的写入当成一条新记录
    state::suppress_monitor_for(4000);

    // 1. 先写剪贴板:此时窗口还在,出错能立刻反馈给前端
    write_clipboard(app, rec)?;
    println!("[粘贴] 剪贴板已写入");

    // 2. 隐藏窗口,把焦点让出去
    crate::hide_main(app);
    let hidden = wait_until_hidden(app, Duration::from_millis(800));
    println!("[粘贴] 主窗口已隐藏: {hidden}");

    // 3. 确保焦点落回"唤起剪贴板之前的那个窗口"
    restore_focus();
    std::thread::sleep(Duration::from_millis(120));

    // 4. 注入 Ctrl+V
    simulate_paste()?;
    println!("[粘贴] Ctrl+V 已发送");
    Ok(())
}

/// 等待窗口真正隐藏(窗口请求是投递到主循环异步处理的)
fn wait_until_hidden(app: &AppHandle, timeout: Duration) -> bool {
    let Some(win) = app.get_window("main") else {
        return true;
    };
    let deadline = Instant::now() + timeout;
    loop {
        if !win.is_visible().unwrap_or(false) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
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
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline && !crate::x11::focus_left_app() {
            std::thread::sleep(Duration::from_millis(25));
        }
        match crate::x11::target_window() {
            Some(win) => {
                let ok = crate::x11::activate_window(win);
                println!("[粘贴] 归还焦点到 0x{win:x},结果={ok}");
            }
            None => println!("[粘贴] 没有记录过目标窗口,依赖窗口管理器归还焦点"),
        }
    }
}

fn simulate_paste() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let result = crate::x11::send_ctrl_v();
    #[cfg(not(target_os = "linux"))]
    let result = simulate_ctrl_v_enigo();
    result
}

/// macOS 预览路径:仍用 enigo(需要辅助功能权限,见 HANDOVER 已知问题)
#[cfg(not(target_os = "linux"))]
fn simulate_ctrl_v_enigo() -> Result<(), String> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};

    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| match e {
        enigo::NewConError::NoPermission => {
            "无输入模拟权限:系统设置 → 隐私与安全性 → 辅助功能,授权给启动本程序的终端(或应用本体)后重启应用".to_string()
        }
        other => format!("创建键盘模拟器失败: {other:?}"),
    })?;

    // macOS 上 Key::Unicode 依赖当前键盘布局查表,中文输入法环境下可能查错键码,
    // 直接用 V 的虚拟键码 9(kVK_ANSI_V)最稳
    let v_key = Key::Other(9);

    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|e| format!("按下 Ctrl 失败: {e:?}"))?;
    enigo
        .key(v_key, Direction::Press)
        .map_err(|e| format!("按下 V 失败: {e:?}"))?;

    std::thread::sleep(Duration::from_millis(8));

    enigo
        .key(v_key, Direction::Release)
        .map_err(|e| format!("释放 V 失败: {e:?}"))?;
    enigo
        .key(Key::Control, Direction::Release)
        .map_err(|e| format!("释放 Ctrl 失败: {e:?}"))?;

    Ok(())
}

fn write_clipboard(app: &AppHandle, rec: &ClipboardRecord) -> Result<(), String> {
    // 复用一个常驻的写上下文:每次新建都会多一条 X 连接 + 一个常驻线程
    clipboard_service::with_writer(|ctx| match rec.kind.as_str() {
        "files" => ctx
            .set_files(rec.files.clone().unwrap_or_default())
            .map_err(|e| format!("写入文件列表失败: {e}")),
        "image" => {
            let img = crate::clipboard_service::load_image(
                app,
                rec.image_path.as_deref().unwrap_or_default(),
            )?;
            // "复制图片文件"来的记录额外带着原始路径:把文件列表一起写进剪贴板,
            // 这样粘到文件管理器还是文件、粘到文档/聊天还是图片
            match rec.files.as_deref().filter(|files| !files.is_empty()) {
                Some(files) => ctx
                    .set(vec![
                        ClipboardContent::Image(img),
                        ClipboardContent::Files(files.to_vec()),
                    ])
                    .map_err(|e| format!("写入图片与文件失败: {e}")),
                None => ctx.set_image(img).map_err(|e| format!("写入图片失败: {e}")),
            }
        }
        "html" => ctx
            .set_html(rec.html.clone().unwrap_or_default())
            .map_err(|e| format!("写入富文本失败: {e}")),
        _ => ctx
            .set_text(rec.text.clone().unwrap_or_default())
            .map_err(|e| format!("写入文本失败: {e}")),
    })
}
