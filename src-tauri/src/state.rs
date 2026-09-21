//! 内存中的剪贴板历史(PoC 阶段;正式迁移时替换为 SQLite 持久化)

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Manager};

/// 一条剪贴板记录
#[derive(Clone, Serialize)]
pub struct ClipboardRecord {
    pub id: u64,
    /// text | html | image | files
    pub kind: String,
    /// 文本内容 / 图片尺寸描述 / 文件名列表
    pub text: Option<String>,
    /// 富文本 HTML 原文
    pub html: Option<String>,
    /// 文件路径列表
    pub files: Option<Vec<String>>,
    /// 相对应用数据目录的图片路径
    pub image_path: Option<String>,
    /// 内容哈希(去重用)
    pub hash: String,
    /// 创建时间(unix 毫秒)
    pub created_at: u64,
}

static HISTORY: Lazy<Mutex<VecDeque<ClipboardRecord>>> =
    Lazy::new(|| Mutex::new(VecDeque::new()));
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SUPPRESS_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

const MAX_HISTORY: usize = 500;
/// 单条文本内容最大保存字符数
const MAX_TEXT_CHARS: usize = 10_000;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 粘贴时抑制监听器,避免把程序自己写入剪贴板的内容当作新记录
pub fn suppress_monitor_for(ms: u64) {
    SUPPRESS_UNTIL_MS.store(now_ms().saturating_add(ms), Ordering::SeqCst);
}

pub fn is_suppressed() -> bool {
    now_ms() < SUPPRESS_UNTIL_MS.load(Ordering::SeqCst)
}

/// 新增(或去重置顶)一条记录并通知前端
pub fn push_and_emit(app: &AppHandle, mut rec: ClipboardRecord) {
    let mut hist = HISTORY.lock();
    // 去重:相同哈希的旧记录移除,新记录置顶
    if let Some(pos) = hist.iter().position(|r| r.hash == rec.hash) {
        hist.remove(pos);
    }
    rec.id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    rec.created_at = now_ms();
    hist.push_front(rec.clone());
    while hist.len() > MAX_HISTORY {
        hist.pop_back();
    }
    drop(hist);
    let _ = app.emit_all("clipboard-updated", rec);
}

pub fn get_all() -> Vec<ClipboardRecord> {
    HISTORY.lock().iter().cloned().collect()
}

pub fn get_by_id(id: u64) -> Option<ClipboardRecord> {
    HISTORY.lock().iter().find(|r| r.id == id).cloned()
}

pub fn clear() {
    HISTORY.lock().clear();
}

pub fn truncate_text(s: &str) -> String {
    if s.chars().count() > MAX_TEXT_CHARS {
        s.chars().take(MAX_TEXT_CHARS).collect()
    } else {
        s.to_string()
    }
}

#[tauri::command]
pub fn cmd_get_history() -> Vec<ClipboardRecord> {
    get_all()
}

#[tauri::command]
pub fn cmd_clear_history() {
    clear();
}
