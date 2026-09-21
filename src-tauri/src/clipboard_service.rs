//! 剪贴板监听与内容捕获(参考 QuickClipboard monitor.rs/capture.rs 简化实现)

use clipboard_rs::{
    common::RustImage, Clipboard, ClipboardContext, ClipboardHandler, ClipboardWatcher,
    ClipboardWatcherContext, RustImageData,
};
use sha2::{Digest, Sha256};
use tauri::AppHandle;

use crate::state::{self, ClipboardRecord};

/// 启动剪贴板监听线程
pub fn start(app: AppHandle) -> Result<(), String> {
    let mut watcher = ClipboardWatcherContext::new()
        .map_err(|e| format!("创建剪贴板监听器失败: {e}"))?;
    watcher.add_handler(ChangeHandler { app });
    std::thread::spawn(move || watcher.start_watch());
    Ok(())
}

struct ChangeHandler {
    app: AppHandle,
}

impl ClipboardHandler for ChangeHandler {
    fn on_clipboard_change(&mut self) {
        if state::is_suppressed() {
            return;
        }
        match capture(&self.app) {
            Ok(Some(rec)) => state::push_and_emit(&self.app, rec),
            Ok(None) => {}
            Err(e) => eprintln!("捕获剪贴板内容失败: {e}"),
        }
    }
}

fn hash_bytes(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

/// 捕获当前剪贴板内容。返回 Ok(None) 表示内容为空或无需记录。
fn capture(app: &AppHandle) -> Result<Option<ClipboardRecord>, String> {
    let ctx = ClipboardContext::new().map_err(|e| e.to_string())?;

    // 优先级:文件 > 图片 > 富文本 > 纯文本
    if let Ok(files) = ctx.get_files() {
        if !files.is_empty() {
            let hash = hash_bytes(files.join("\u{0}").as_bytes());
            let text = files
                .iter()
                .map(|f| {
                    std::path::Path::new(f)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| f.clone())
                })
                .collect::<Vec<_>>()
                .join("、");
            return Ok(Some(ClipboardRecord {
                id: 0,
                kind: "files".into(),
                text: Some(text),
                html: None,
                files: Some(files),
                image_path: None,
                hash,
                created_at: 0,
            }));
        }
    }

    if let Ok(image) = ctx.get_image() {
        let (rel_path, hash) = save_image(app, &image)?;
        let (w, h) = image.get_size();
        return Ok(Some(ClipboardRecord {
            id: 0,
            kind: "image".into(),
            text: Some(format!("图片 {w}×{h}")),
            html: None,
            files: None,
            image_path: Some(rel_path),
            hash,
            created_at: 0,
        }));
    }

    let html = ctx.get_html().ok();
    let text = ctx.get_text().ok();
    match (html, text) {
        (Some(_), None) | (None, None) => Ok(None),
        (html, Some(text)) => {
            if text.trim().is_empty() && html.is_none() {
                return Ok(None);
            }
            let kind = if html.is_some() { "html" } else { "text" };
            let hash = hash_bytes(
                html.clone()
                    .unwrap_or_else(|| text.clone())
                    .as_bytes(),
            );
            Ok(Some(ClipboardRecord {
                id: 0,
                kind: kind.into(),
                text: Some(state::truncate_text(&text)),
                html: html.map(|h| state::truncate_text(&h)),
                files: None,
                image_path: None,
                hash,
                created_at: 0,
            }))
        }
    }
}

/// 保存剪贴板图片为 PNG,返回(相对路径, 哈希)
fn save_image(app: &AppHandle, image: &RustImageData) -> Result<(String, String), String> {
    use image::{codecs::png::PngEncoder, ImageEncoder};

    let rgba = image.to_rgba8().map_err(|e| e.to_string())?;
    let (w, h) = (rgba.width(), rgba.height());

    let mut png_data = Vec::new();
    PngEncoder::new(&mut png_data)
        .write_image(
            rgba.as_raw(),
            w,
            h,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;

    let hash = hash_bytes(&png_data);
    let filename = format!("{}.png", &hash[..16]);

    let images_dir = app
        .path_resolver()
        .app_data_dir()
        .ok_or("无法定位应用数据目录")?
        .join("clipboard_images");
    std::fs::create_dir_all(&images_dir).map_err(|e| format!("创建目录失败: {e}"))?;

    let final_path = images_dir.join(&filename);
    if !final_path.exists() {
        std::fs::write(&final_path, &png_data).map_err(|e| format!("写入图片失败: {e}"))?;
    }

    Ok((format!("clipboard_images/{filename}"), hash))
}

/// 从数据目录读取图片(粘贴回写剪贴板用)
pub fn load_image(app: &AppHandle, rel_path: &str) -> Result<RustImageData, String> {
    let path = app
        .path_resolver()
        .app_data_dir()
        .ok_or("无法定位应用数据目录")?
        .join(rel_path);
    let bytes = std::fs::read(&path).map_err(|e| format!("读取图片失败: {e}"))?;
    RustImageData::from_bytes(&bytes).map_err(|e| e.to_string())
}
