//! 剪贴板监听与内容捕获(参考 QuickClipboard monitor.rs/capture.rs 简化实现)

use clipboard_rs::{
    common::RustImage, Clipboard, ClipboardContext, ClipboardHandler, ClipboardWatcher,
    ClipboardWatcherContext, RustImageData,
};
#[cfg(target_os = "linux")]
use once_cell::sync::Lazy;
#[cfg(target_os = "linux")]
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tauri::AppHandle;

use crate::state::{self, ClipboardRecord};

// 进程级复用的剪贴板上下文。
//
// Linux(X11)下 clipboard-rs 每次 `ClipboardContext::new()` 都会建立 2 条 X 连接并
// 启动一个永不退出的服务线程。监听线程在每次剪贴板变化时都要读一次剪贴板,如果每次
// 都新建上下文,复制几百次后就会耗光 X 服务器的客户端名额(默认 256),之后连建立
// 新连接都会失败,剪贴板读写随之全部瘫痪。所以读/写各复用一份。
//
// macOS 的上下文只是一个 pasteboard 句柄且不满足跨线程共享要求,不做缓存。
#[cfg(target_os = "linux")]
static READ_CTX: Lazy<Mutex<Option<ClipboardContext>>> = Lazy::new(|| Mutex::new(None));
#[cfg(target_os = "linux")]
static WRITE_CTX: Lazy<Mutex<Option<ClipboardContext>>> = Lazy::new(|| Mutex::new(None));

/// 借用复用的"读"上下文执行一次操作
pub fn with_reader<T>(f: impl FnOnce(&ClipboardContext) -> Result<T, String>) -> Result<T, String> {
    #[cfg(target_os = "linux")]
    {
        let mut guard = READ_CTX.lock();
        if guard.is_none() {
            *guard = Some(
                ClipboardContext::new().map_err(|e| format!("创建剪贴板读上下文失败: {e}"))?,
            );
        }
        let ctx: &ClipboardContext = guard.as_ref().expect("上下文刚创建过");
        f(ctx)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let ctx = ClipboardContext::new().map_err(|e| format!("创建剪贴板读上下文失败: {e}"))?;
        f(&ctx)
    }
}

/// 借用复用的"写"上下文执行一次操作(粘贴时写回剪贴板)
pub fn with_writer<T>(f: impl FnOnce(&ClipboardContext) -> Result<T, String>) -> Result<T, String> {
    #[cfg(target_os = "linux")]
    {
        let mut guard = WRITE_CTX.lock();
        if guard.is_none() {
            *guard = Some(
                ClipboardContext::new().map_err(|e| format!("创建剪贴板写上下文失败: {e}"))?,
            );
        }
        let ctx: &ClipboardContext = guard.as_ref().expect("上下文刚创建过");
        f(ctx)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let ctx = ClipboardContext::new().map_err(|e| format!("创建剪贴板写上下文失败: {e}"))?;
        f(&ctx)
    }
}

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
    with_reader(|ctx| capture_with(app, ctx))
}

fn capture_with(
    app: &AppHandle,
    ctx: &ClipboardContext,
) -> Result<Option<ClipboardRecord>, String> {
    // 来源应用:剪贴板变化事件通常紧跟复制动作,此刻的活动窗口就是源应用
    let source_app = current_source_app();

    // 优先级:文件 > 文字/富文本(仅当富文本里确实有文字)> 图片 > 纯文本
    if let Ok(files) = ctx.get_files() {
        if !files.is_empty() {
            // 只复制了一个图片文件 → 按图片处理(展示缩略图、归入"图片"分类,标题用文件名),
            // 同时保留原始文件路径,粘贴时会把图片和文件列表一起写进剪贴板
            if files.len() == 1 {
                if let Some(rec) = capture_image_file(app, &files[0], source_app.clone()) {
                    return Ok(Some(rec));
                }
            }

            let hash = hash_bytes(files.join("\u{0}").as_bytes());
            return Ok(Some(ClipboardRecord {
                id: 0,
                kind: "files".into(),
                text: Some(file_title(&files)),
                html: None,
                files: Some(files),
                image_path: None,
                hash,
                created_at: 0,
                source_app,
                group: None,
            }));
        }
    }

    // 注意:clipboard-rs 在 X11 下会把"目标格式读不到"统一转成 Ok("")/空串,
    // 所以这里必须把空内容归一成 None。否则遇到只提供 image/bmp 这类非 PNG 图片、
    // 或者只提供私有格式的剪贴板时,会凭空多出一条空白记录(实测 Xvfb + image/bmp 可复现)。
    let html = ctx.get_html().ok().filter(|h| !h.trim().is_empty());
    let text = ctx.get_text().ok().filter(|t| !t.trim().is_empty());

    // 文字优先于图片:Word/LibreOffice/WPS 复制**文字**时,还会顺带在剪贴板放一份
    // "渲染成位图"的副本(image/png 或 image/bmp),光看"有没有图"会把复制文字误判成截图。
    // 只有"没有可见文字"(截图工具、或富文本里只有一个 <img>:浏览器右键复制图片)才按图片记录。
    // 这个优先级与 ref 项目 QuickClipboard 一致(见其 capture.rs:文字/富文本在前,图片最后收敛)。
    if text.is_some() || html.as_deref().is_some_and(html_has_visible_text) {
        return Ok(Some(text_record(html, text, source_app)));
    }

    if let Ok(image) = ctx.get_image() {
        let (rel_path, hash) = save_image(app, &image)?;
        let (w, h) = image.get_size();
        return Ok(Some(ClipboardRecord {
            id: 0,
            kind: "image".into(),
            // 剪贴板里的图像数据(截图工具输出的就是这种):标题标明是截图并带上尺寸
            text: Some(format!("截图「{w} × {h}」")),
            html: None,
            files: None,
            image_path: Some(rel_path),
            hash,
            created_at: 0,
            source_app,
            group: None,
        }));
    }

    if html.is_none() && text.is_none() {
        return Ok(None);
    }
    Ok(Some(text_record(html, text, source_app)))
}

/// 组装一条文字/富文本记录:文本给标题,富文本留着粘贴成带格式的内容
fn text_record(
    html: Option<String>,
    text: Option<String>,
    source_app: Option<String>,
) -> ClipboardRecord {
    let kind = if html.is_some() { "html" } else { "text" };
    let text = text.unwrap_or_default();
    let hash = hash_bytes(html.clone().unwrap_or_else(|| text.clone()).as_bytes());
    ClipboardRecord {
        id: 0,
        kind: kind.into(),
        text: Some(state::truncate_text(&text)),
        html: html.map(|h| state::truncate_text(&h)),
        files: None,
        image_path: None,
        hash,
        created_at: 0,
        source_app,
        group: None,
    }
}

/// 富文本里是否有"真正的文字":去掉标签、把 `&nbsp;` 当空白之后还剩字母/数字/CJK。
///
/// 用来区分:Word 复制文字(富文本里有 `<p>正文</p>` + 一张位图)vs 浏览器复制图片
/// (富文本里常常只有一个 `<img src=...>`,没有文字)。
fn html_has_visible_text(html: &str) -> bool {
    let mut plain = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => plain.push(ch),
            _ => {}
        }
    }
    plain
        .replace("&nbsp;", " ")
        .chars()
        .any(|c| c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::html_has_visible_text;

    /// Word/WPS 复制文字:富文本里有正文 → 必须按文字处理(哪怕剪贴板上还挂着位图)
    #[test]
    fn rich_text_with_text_counts_as_text() {
        assert!(html_has_visible_text("<p class=MsoNormal>你好 world</p>"));
        assert!(html_has_visible_text("<table><tr><td>42</td></tr></table>"));
        assert!(html_has_visible_text("没有标签的纯文本片段"));
    }

    /// 浏览器右键"复制图片":富文本里只有 <img> 或空标签 → 仍然是图片
    #[test]
    fn image_only_rich_text_counts_as_image() {
        assert!(!html_has_visible_text(
            "<meta charset='utf-8'><img src=\"https://a.example/b.png\" alt=\"\">"
        ));
        assert!(!html_has_visible_text("<p>&nbsp;</p><img src=\"a.png\">"));
        assert!(!html_has_visible_text("<div><br/></div>"));
    }
}

/// 当前前台应用(macOS 走 NSWorkspace,Linux 走 X11 活动窗口;见 appinfo.rs)
fn current_source_app() -> Option<String> {
    crate::appinfo::active_app_name()
}

/// 能按图片处理的文件扩展名(与 image crate 已启用的解码器一致)
const IMAGE_EXTS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "tiff", "tif"];
/// 超过这个体积的图片文件不做解码/复制,仍按普通文件记录(避免捕获线程长时间占用内存)
const MAX_IMAGE_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// 把"被复制的图片文件"转成图片记录:标题用文件名,便于和截图区分。
/// 解析/解码任何一步失败都返回 None,调用方按普通文件记录处理。
fn capture_image_file(
    app: &AppHandle,
    raw: &str,
    source_app: Option<String>,
) -> Option<ClipboardRecord> {
    let path = local_path(raw)?;
    let ext = path
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    if !IMAGE_EXTS.contains(&ext.as_str()) {
        return None;
    }

    let size = std::fs::metadata(&path).ok()?.len();
    if size > MAX_IMAGE_FILE_BYTES {
        println!(
            "[捕获] 图片文件过大({} MB),按普通文件记录: {}",
            size / 1024 / 1024,
            path.display()
        );
        return None;
    }

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("[捕获] 读取图片文件失败({e}),按普通文件记录: {}", path.display());
            return None;
        }
    };
    let image = match RustImageData::from_bytes(&bytes) {
        Ok(image) => image,
        Err(e) => {
            eprintln!("[捕获] 解码图片文件失败({e}),按普通文件记录: {}", path.display());
            return None;
        }
    };
    let (rel_path, hash) = match save_image(app, &image) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[捕获] 保存图片失败({e}),按普通文件记录");
            return None;
        }
    };

    let title = file_title(&[raw.to_string()]);
    println!("[捕获] 图片文件 {title} → {rel_path}");
    Some(ClipboardRecord {
        id: 0,
        kind: "image".into(),
        text: Some(title),
        html: None,
        // 保留原始路径:粘贴时会把文件列表一起写进剪贴板
        files: Some(vec![raw.to_string()]),
        image_path: Some(rel_path),
        hash,
        created_at: 0,
        source_app,
        group: None,
    })
}

/// 列表标题:`文件名「所在目录」`;多个文件用 `首个 等 N 个「目录」`。
/// 目录过长由前端 CSS 省略号处理(文件名在前,保证不会被截掉)。
fn file_title(files: &[String]) -> String {
    let (name, dir) = display_name_and_dir(&files[0]);
    let head = if files.len() > 1 {
        format!("{name} 等 {} 个", files.len())
    } else {
        name
    };
    if dir.is_empty() {
        head
    } else {
        format!("{head}「{dir}」")
    }
}

/// 把剪贴板里的路径/URI 拆成展示用的 (文件名, 所在目录)
fn display_name_and_dir(raw: &str) -> (String, String) {
    let path = local_path(raw).unwrap_or_else(|| std::path::PathBuf::from(raw));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| raw.to_string());
    let dir = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    (name, dir)
}

/// 把剪贴板里的 `file:///home/a%20b.png` 还原成本地路径
fn local_path(raw: &str) -> Option<std::path::PathBuf> {
    let path = if let Some(rest) = raw.strip_prefix("file://") {
        // file://host/path:取第一个 '/' 之后的部分(本地文件一般 host 为空)
        let idx = rest.find('/')?;
        &rest[idx..]
    } else {
        raw
    };
    Some(std::path::PathBuf::from(percent_decode(path)))
}

/// 只处理 %XX 转义(路径里的空格、中文等)
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push(high * 16 + low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 保存剪贴板图片为 PNG(同时生成列表用的缩略图),返回(相对路径, 哈希)
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

    let data_dir = app
        .path_resolver()
        .app_data_dir()
        .ok_or("无法定位应用数据目录")?;
    let images_dir = data_dir.join(state::IMAGES_DIR);

    // 原图:文件名就是内容哈希,同一张图只写一次
    let final_path = images_dir.join(&filename);
    if !final_path.exists() {
        std::fs::create_dir_all(&images_dir).map_err(|e| format!("创建目录失败: {e}"))?;
        std::fs::write(&final_path, &png_data).map_err(|e| format!("写入图片失败: {e}"))?;
    }

    // 缩略图:列表里每行只画 38px,4K 截图直接塞给 webview 会吃掉几十 MB 内存,
    // 所以这里顺手生成一张长边 240px 的小图(此时图已经解码在内存里,不额外付代价)
    save_thumbnail(&data_dir, &hash, image);

    Ok((format!("{}/{filename}", state::IMAGES_DIR), hash))
}

/// 生成缩略图,失败只记日志不影响记录本身
fn save_thumbnail(data_dir: &std::path::Path, hash: &str, image: &RustImageData) {
    let thumbs_dir = data_dir.join(state::THUMBS_DIR);
    let thumb_path = thumbs_dir.join(format!("{}.png", &hash[..16]));
    if thumb_path.exists() {
        return;
    }
    let result = (|| -> Result<(), String> {
        let thumb = image.thumbnail(240, 240).map_err(|e| e.to_string())?;
        let png = thumb.to_png().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&thumbs_dir).map_err(|e| format!("创建缩略图目录失败: {e}"))?;
        std::fs::write(&thumb_path, png.get_bytes()).map_err(|e| format!("写缩略图失败: {e}"))
    })();
    if let Err(e) = result {
        eprintln!("[捕获] 生成缩略图失败({e}): {}", thumb_path.display());
    }
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
