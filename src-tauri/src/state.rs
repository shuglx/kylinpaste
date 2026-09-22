//! 剪贴板历史:内存保存 + JSON 落盘(PoC 阶段;正式迁移时替换为 SQLite)

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use tauri::{AppHandle, Manager};

/// 一条剪贴板记录
#[derive(Clone, Serialize, Deserialize)]
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
    /// 来源应用(复制时前台应用的显示名,如 "CodeBuddy CN"、"firefox")。
    /// 老的历史文件里没有这些字段,所以都给默认值。
    #[serde(default)]
    pub source_app: Option<String>,
    /// 所属分组(每条记录最多一个);None = 未分组
    #[serde(default)]
    pub group: Option<String>,
    /// 收藏。收藏和分组的记录**不占用**"保留条数"上限,也不会被自动淘汰
    #[serde(default)]
    pub favorite: bool,
}

/// 落盘文件结构(app_data_dir/history.json)
#[derive(Serialize, Deserialize)]
struct PersistFile {
    version: u32,
    items: Vec<ClipboardRecord>,
}

static HISTORY: Lazy<Mutex<VecDeque<ClipboardRecord>>> =
    Lazy::new(|| Mutex::new(VecDeque::new()));
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SUPPRESS_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// 有改动待落盘
static DIRTY: AtomicBool = AtomicBool::new(false);
/// 应用数据目录(启动时确定)
static DATA_DIR: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));

/// 保留的"临时记录"条数上限(默认 500,设置界面可改)。
/// 收藏与分组的记录不计入这个数,也不会被淘汰,所以总量可能超过它。
const MAX_HISTORY_DEFAULT: usize = 500;
static MAX_ITEMS: AtomicUsize = AtomicUsize::new(MAX_HISTORY_DEFAULT);

/// 当前保留条数上限
pub fn max_items() -> usize {
    MAX_ITEMS.load(Ordering::SeqCst)
}

/// 设置上限并立即按新上限裁剪(设置界面改"保留条目总数"时调用)
pub fn set_max_items(limit: usize) {
    MAX_ITEMS.store(limit.max(1), Ordering::SeqCst);
    let mut hist = HISTORY.lock();
    trim(&mut hist, max_items());
    drop(hist);
    mark_dirty();
}
/// 图片目录(相对应用数据目录)
pub const IMAGES_DIR: &str = "clipboard_images";
/// 缩略图目录(列表里展示用,长边 240px)
pub const THUMBS_DIR: &str = "clipboard_images/thumbs";
/// 单条文本内容最大保存字符数
const MAX_TEXT_CHARS: usize = 10_000;
/// 分组名最大字符数(超长截断,避免前端被撑爆)
const MAX_GROUP_CHARS: usize = 32;
const HISTORY_FILE: &str = "history.json";
const PERSIST_VERSION: u32 = 1;
/// 变更后延迟多久落盘(连续复制只写一次)
const SAVE_DEBOUNCE_MS: u64 = 500;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------- 持久化

/// 启动时载入历史,并开启"改动后延迟落盘"的后台线程
pub fn start_persistence(app: &AppHandle) {
    match app.path_resolver().app_data_dir() {
        Some(dir) => {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("[持久化] 创建数据目录失败: {e}");
            }
            *DATA_DIR.lock() = Some(dir);
        }
        None => eprintln!("[持久化] 无法定位应用数据目录,历史只在本次运行内有效"),
    }

    load();

    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_millis(SAVE_DEBOUNCE_MS));
        if DIRTY.swap(false, Ordering::SeqCst) {
            if let Err(e) = save() {
                eprintln!("[持久化] 写入历史失败: {e}");
                DIRTY.store(true, Ordering::SeqCst); // 下次再试
            }
        }
    });
}

pub fn history_path() -> Option<PathBuf> {
    DATA_DIR.lock().as_ref().map(|dir| dir.join(HISTORY_FILE))
}

fn mark_dirty() {
    DIRTY.store(true, Ordering::SeqCst);
}

fn load() {
    let Some(path) = history_path() else {
        return;
    };
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(_) => {
            println!("[持久化] 没有历史文件,本次从空开始: {}", path.display());
            return;
        }
    };
    match serde_json::from_slice::<PersistFile>(&raw) {
        Ok(file) => {
            let mut hist = HISTORY.lock();
            hist.clear();
            for rec in file.items {
                hist.push_back(rec);
            }
            let before = hist.len();
            trim(&mut hist, max_items());
            let max_id = hist.iter().map(|r| r.id).max().unwrap_or(0);
            NEXT_ID.store(max_id + 1, Ordering::SeqCst);
            println!(
                "[持久化] 载入 {} 条历史(临时上限 {},裁掉 {})",
                hist.len(),
                max_items(),
                before - hist.len()
            );
        }
        Err(e) => eprintln!(
            "[持久化] 历史文件解析失败({e}),本次从空开始: {}",
            path.display()
        ),
    }
}

fn save() -> Result<(), String> {
    let Some(path) = history_path() else {
        return Ok(());
    };
    let items: Vec<ClipboardRecord> = HISTORY.lock().iter().cloned().collect();
    let json = serde_json::to_vec(&PersistFile {
        version: PERSIST_VERSION,
        items,
    })
    .map_err(|e| e.to_string())?;

    // 先写临时文件再改名:避免写到一半中断把历史写坏
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| format!("写入 {tmp:?} 失败: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("替换 {path:?} 失败: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------- 记忆(抑制监听)

/// 粘贴时抑制监听器,避免把程序自己写入剪贴板的内容当作新记录
pub fn suppress_monitor_for(ms: u64) {
    SUPPRESS_UNTIL_MS.store(now_ms().saturating_add(ms), Ordering::SeqCst);
}

pub fn is_suppressed() -> bool {
    now_ms() < SUPPRESS_UNTIL_MS.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------- 历史读写

/// 去重插入一条记录,返回带新 id/时间的记录。
///
/// 相同哈希的旧记录移除、新记录置顶;**新记录没有分组时继承旧记录的分组**,
/// 这样同一份内容被重新复制不会把用户打过的分组弄丢(收藏记在前端,按哈希天然保留)。
fn insert_record(hist: &mut VecDeque<ClipboardRecord>, mut rec: ClipboardRecord) -> ClipboardRecord {
    if let Some(pos) = hist.iter().position(|r| r.hash == rec.hash) {
        let old = hist.remove(pos).expect("position 刚返回过");
        if rec.group.is_none() {
            rec.group = old.group;
        }
        // 收藏过的内容重新复制,仍然是收藏
        rec.favorite |= old.favorite;
    }
    rec.id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    rec.created_at = now_ms();
    hist.push_front(rec.clone());
    trim(hist, max_items());
    rec
}

/// 一条记录是不是"临时记录"(既没收藏也没分组)——只有这种才会被上限淘汰
fn is_transient(rec: &ClipboardRecord) -> bool {
    !rec.favorite && rec.group.is_none()
}

/// 按"临时记录不超过 limit 条"裁剪:只从最旧的一端淘汰临时记录,
/// 收藏/分组过的记录一律保留(与界面上"清理临时记录"的规则保持一致)。
fn trim(hist: &mut VecDeque<ClipboardRecord>, limit: usize) {
    let transient = hist.iter().filter(|rec| is_transient(rec)).count();
    let mut excess = transient.saturating_sub(limit);
    let mut index = hist.len();
    while excess > 0 && index > 0 {
        index -= 1;
        if is_transient(&hist[index]) {
            hist.remove(index);
            excess -= 1;
        }
    }
}

/// 新增(或去重置顶)一条记录并通知前端
pub fn push_and_emit(app: &AppHandle, rec: ClipboardRecord) {
    let rec = insert_record(&mut HISTORY.lock(), rec);
    mark_dirty();
    let _ = app.emit_all("clipboard-updated", rec);
}

pub fn get_all() -> Vec<ClipboardRecord> {
    HISTORY.lock().iter().cloned().collect()
}

pub fn get_by_id(id: u64) -> Option<ClipboardRecord> {
    HISTORY.lock().iter().find(|r| r.id == id).cloned()
}

/// 设置(或取消)收藏。收藏后不占"保留条数"上限,也不会被自动淘汰。
pub fn set_favorite(id: u64, favorite: bool) -> Option<ClipboardRecord> {
    let mut hist = HISTORY.lock();
    let rec = hist.iter_mut().find(|r| r.id == id)?;
    rec.favorite = favorite;
    let updated = rec.clone();
    drop(hist);
    mark_dirty();
    Some(updated)
}

/// 迁移用:把"早先存在前端 localStorage 里、按内容哈希记的收藏"补到记录上。
/// 返回被打上收藏的条数(幂等,重复调用不会有副作用)。
pub fn import_favorites(hashes: &[String]) -> usize {
    if hashes.is_empty() {
        return 0;
    }
    let mut hist = HISTORY.lock();
    let mut marked = 0;
    for rec in hist.iter_mut() {
        if !rec.favorite && hashes.iter().any(|h| *h == rec.hash) {
            rec.favorite = true;
            marked += 1;
        }
    }
    drop(hist);
    if marked > 0 {
        mark_dirty();
    }
    marked
}

// ---------------------------------------------------------------- 分组 / 删除

/// 设置(或清除)一条记录的分组。空白字符串一律当作清除,超长截断。
pub fn set_group(id: u64, group: Option<String>) -> Option<ClipboardRecord> {
    let group = group
        .map(|g| {
            let g = g.trim();
            g.chars().take(MAX_GROUP_CHARS).collect::<String>()
        })
        .filter(|g| !g.is_empty());

    let mut hist = HISTORY.lock();
    let rec = hist.iter_mut().find(|r| r.id == id)?;
    rec.group = group;
    let updated = rec.clone();
    drop(hist);
    mark_dirty();
    Some(updated)
}

/// 删除若干条记录(连同磁盘上属于它们的图片与缩略图),返回实际删除的条数。
///
/// 调用方负责决定"哪些该删"(收藏在 localStorage 里、分组在记录里,
/// 所以"清空但保留收藏/分组"的策略由前端算好 ids 传进来)。
pub fn remove_ids(ids: &[u64]) -> usize {
    if ids.is_empty() {
        return 0;
    }
    let mut hist = HISTORY.lock();
    let old = std::mem::take(&mut *hist);
    let mut removed: Vec<ClipboardRecord> = Vec::new();
    let mut kept = VecDeque::with_capacity(old.len());
    for rec in old {
        if ids.contains(&rec.id) {
            removed.push(rec);
        } else {
            kept.push_back(rec);
        }
    }
    // 还留在历史里的图片路径:这些文件不能删(同一张图只可能有一条记录,
    // 但这里再兜一层,避免以后改了去重逻辑就误删)
    let alive: Vec<String> = kept.iter().filter_map(|r| r.image_path.clone()).collect();
    *hist = kept;
    drop(hist);

    for rec in &removed {
        remove_record_files(rec, &alive);
    }
    if !removed.is_empty() {
        mark_dirty();
    }
    removed.len()
}

/// 删掉记录在应用数据目录里的图片与缩略图(用户自己的原文件绝不碰)
fn remove_record_files(rec: &ClipboardRecord, alive: &[String]) {
    let Some(dir) = DATA_DIR.lock().clone() else {
        return;
    };
    if let Some(rel) = &rec.image_path {
        if !alive.iter().any(|p| p == rel) {
            let _ = std::fs::remove_file(dir.join(rel));
        }
    }
    if let Some(prefix) = rec.hash.get(..16) {
        let _ = std::fs::remove_file(dir.join(THUMBS_DIR).join(format!("{prefix}.png")));
    }
}

pub fn truncate_text(s: &str) -> String {
    if s.chars().count() > MAX_TEXT_CHARS {
        s.chars().take(MAX_TEXT_CHARS).collect()
    } else {
        s.to_string()
    }
}

/// 图片资源目录(前端用 asset 协议 + convertFileSrc 加载缩略图)
#[derive(Serialize)]
pub struct AssetDirs {
    /// 应用数据目录,拼接记录里的 `image_path` 即原图绝对路径
    pub data_dir: String,
    /// 缩略图目录,文件名 = `<hash 前16位>.png`
    pub thumbs_dir: String,
}

#[tauri::command]
pub fn cmd_get_asset_dirs(app: AppHandle) -> Option<AssetDirs> {
    let data_dir = app.path_resolver().app_data_dir()?;
    Some(AssetDirs {
        data_dir: data_dir.to_string_lossy().into_owned(),
        thumbs_dir: data_dir.join(THUMBS_DIR).to_string_lossy().into_owned(),
    })
}

#[tauri::command]
pub fn cmd_get_history() -> Vec<ClipboardRecord> {
    get_all()
}

/// 收藏 / 取消收藏一条记录
#[tauri::command]
pub fn cmd_set_favorite(id: u64, favorite: bool) -> Result<ClipboardRecord, String> {
    set_favorite(id, favorite).ok_or_else(|| "记录不存在".to_string())
}

/// 把前端旧版存在 localStorage 里的收藏导入到记录上(启动时调一次)
#[tauri::command]
pub fn cmd_import_favorites(hashes: Vec<String>) -> usize {
    import_favorites(&hashes)
}

/// 给一条记录指定分组(传 null/空串 = 取消分组)
#[tauri::command]
pub fn cmd_set_group(id: u64, group: Option<String>) -> Result<ClipboardRecord, String> {
    set_group(id, group).ok_or_else(|| "记录不存在".to_string())
}

/// 删除若干条记录(含磁盘图片),返回实际删除的条数
#[tauri::command]
pub fn cmd_delete_records(ids: Vec<u64>) -> usize {
    remove_ids(&ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(hash: &str, text: &str) -> ClipboardRecord {
        ClipboardRecord {
            id: 0,
            kind: "text".into(),
            text: Some(text.into()),
            html: None,
            files: None,
            image_path: None,
            hash: hash.into(),
            created_at: 0,
            source_app: None,
            group: None,
            favorite: false,
        }
    }

    /// 插入到全局历史(测试用;各用例的 hash/id 都不重复,可并行跑)
    fn insert(hash: &str, text: &str) -> ClipboardRecord {
        insert_record(&mut HISTORY.lock(), rec(hash, text))
    }

    fn ids() -> Vec<u64> {
        HISTORY.lock().iter().map(|r| r.id).collect()
    }

    /// 同一份内容重新复制:分组与收藏都跟着内容走,不会因为去重置顶而丢
    #[test]
    fn dedupe_inherits_group_and_favorite() {
        let first = insert("h-dedupe", "hello");
        set_group(first.id, Some("work".into()));
        set_favorite(first.id, true);

        let second = insert("h-dedupe", "hello");
        assert_eq!(second.group.as_deref(), Some("work"));
        assert!(second.favorite, "收藏过的内容重新复制仍是收藏");

        let hist = HISTORY.lock();
        let same: Vec<&ClipboardRecord> = hist.iter().filter(|r| r.hash == "h-dedupe").collect();
        assert_eq!(same.len(), 1, "同哈希只应保留一条");
        assert_eq!(same[0].id, second.id, "新记录应顶到最前");
    }

    /// 保留条数上限只管"临时记录":收藏与分组的一律不淘汰(与界面"清理"规则一致)
    #[test]
    fn trim_spares_favorite_and_grouped() {
        let mut hist = VecDeque::new();
        // 越早插入的越旧:临时、收藏、分组、临时
        let oldest = insert_record(&mut hist, rec("h-t1", "1"));
        let fav = insert_record(&mut hist, rec("h-fav", "2"));
        let grp = insert_record(&mut hist, rec("h-grp", "3"));
        let newest = insert_record(&mut hist, rec("h-t2", "4"));
        for r in hist.iter_mut() {
            if r.id == fav.id {
                r.favorite = true;
            }
            if r.id == grp.id {
                r.group = Some("web".into());
            }
        }

        trim(&mut hist, 1); // 只允许 1 条临时记录

        let ids: Vec<u64> = hist.iter().map(|r| r.id).collect();
        assert!(ids.contains(&fav.id), "收藏的留下");
        assert!(ids.contains(&grp.id), "分组的留下");
        assert!(!ids.contains(&oldest.id), "最旧的临时记录先淘汰");
        assert!(ids.contains(&newest.id), "最新的临时记录保留");
        assert_eq!(hist.iter().filter(|r| is_transient(r)).count(), 1);
    }

    /// 分组名:去空白、超长截断、空白等于取消分组
    #[test]
    fn set_group_normalizes() {
        let r = insert("h-group", "x");

        assert_eq!(
            set_group(r.id, Some("  web  ".into())).unwrap().group.as_deref(),
            Some("web")
        );
        assert_eq!(set_group(r.id, Some("   ".into())).unwrap().group, None);
        assert_eq!(
            set_group(r.id, Some("a".repeat(80)))
                .unwrap()
                .group
                .unwrap()
                .chars()
                .count(),
            MAX_GROUP_CHARS
        );
        assert!(
            set_group(999_999_999, Some("x".into())).is_none(),
            "id 不存在时返回 None"
        );
    }

    /// 删除:只删给到的 id,其它记录不受影响
    #[test]
    fn remove_ids_is_selective() {
        let a = insert("h-a", "a");
        let b = insert("h-b", "b");
        let c = insert("h-c", "c");

        assert_eq!(remove_ids(&[b.id]), 1);
        let left = ids();
        assert!(left.contains(&a.id) && left.contains(&c.id));
        assert!(!left.contains(&b.id));
        assert_eq!(remove_ids(&[]), 0, "空列表不做任何事");
    }
}
