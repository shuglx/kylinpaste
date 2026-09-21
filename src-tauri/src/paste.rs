//! 粘贴:写回剪贴板 + 模拟 Ctrl+V(参考 QuickClipboard paste/keyboard.rs)

use clipboard_rs::{Clipboard, ClipboardContext};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use tauri::AppHandle;

use crate::state::{self, ClipboardRecord};

#[tauri::command]
pub fn cmd_paste_item(app: AppHandle, id: u64) -> Result<(), String> {
    let rec = state::get_by_id(id).ok_or("记录不存在")?;
    paste_record(&app, &rec)
}

/// 把记录粘贴回之前的应用:
/// 1. 隐藏主窗口(焦点交还目标应用)
/// 2. 内容写回剪贴板
/// 3. 模拟 Ctrl+V
pub fn paste_record(app: &AppHandle, rec: &ClipboardRecord) -> Result<(), String> {
    println!(
        "[粘贴] 开始: 类型={} 哈希前8={}",
        rec.kind,
        &rec.hash[..rec.hash.len().min(8)]
    );

    // 抑制监听器,避免把自己写入的内容当作新记录
    state::suppress_monitor_for(2000);

    // 隐藏窗口,等待窗口管理器把焦点还给目标应用
    // (app.hide 的消息是异步投递到主线程的,留足处理时间)
    crate::hide_main(app);
    std::thread::sleep(std::time::Duration::from_millis(350));

    write_clipboard(app, rec)?;
    println!("[粘贴] 剪贴板已写入");
    std::thread::sleep(std::time::Duration::from_millis(120));

    simulate_ctrl_v()?;
    println!("[粘贴] Ctrl+V 已发送");
    Ok(())
}

fn write_clipboard(app: &AppHandle, rec: &ClipboardRecord) -> Result<(), String> {
    let ctx = ClipboardContext::new().map_err(|e| e.to_string())?;
    match rec.kind.as_str() {
        "files" => ctx
            .set_files(rec.files.clone().unwrap_or_default())
            .map_err(|e| format!("写入文件列表失败: {e}")),
        "image" => {
            let img = crate::clipboard_service::load_image(
                app,
                rec.image_path.as_deref().unwrap_or_default(),
            )?;
            ctx.set_image(img).map_err(|e| format!("写入图片失败: {e}"))
        }
        "html" => ctx
            .set_html(rec.html.clone().unwrap_or_default())
            .map_err(|e| format!("写入富文本失败: {e}")),
        _ => ctx
            .set_text(rec.text.clone().unwrap_or_default())
            .map_err(|e| format!("写入文本失败: {e}")),
    }
}

/// 模拟 Ctrl+V(Windows 之外的平台通用路径,与 QuickClipboard 一致)
fn simulate_ctrl_v() -> Result<(), String> {
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| match e {
        enigo::NewConError::NoPermission => {
            "无输入模拟权限:系统设置 → 隐私与安全性 → 辅助功能,授权给启动本程序的终端(或应用本体)后重启应用".to_string()
        }
        other => format!("创建键盘模拟器失败: {other:?}"),
    })?;

    // macOS 上 Key::Unicode 依赖当前键盘布局查表,中文输入法环境下可能查错键码,
    // 直接用 V 的虚拟键码 9(kVK_ANSI_V)最稳;
    // Linux 走 Unicode 路径(xkbcommon 查 keysym,无此问题)
    #[cfg(target_os = "macos")]
    let v_key = Key::Other(9);
    #[cfg(not(target_os = "macos"))]
    let v_key = Key::Unicode('v');

    enigo.key(Key::Control, Direction::Press)
        .map_err(|e| format!("按下 Ctrl 失败: {e:?}"))?;
    enigo.key(v_key, Direction::Press)
        .map_err(|e| format!("按下 V 失败: {e:?}"))?;

    std::thread::sleep(std::time::Duration::from_millis(8));

    enigo.key(v_key, Direction::Release)
        .map_err(|e| format!("释放 V 失败: {e:?}"))?;
    enigo.key(Key::Control, Direction::Release)
        .map_err(|e| format!("释放 Ctrl 失败: {e:?}"))?;

    Ok(())
}
