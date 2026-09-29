//! 剪贴板历史:内存保存 + JSON 落盘(PoC 阶段;正式迁移时替换为 SQLite)

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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

/// 发给前端的记录视图。
///
/// 与 `ClipboardRecord` 的区别:
/// - **不含 `html`**:富文本原文只在粘贴回写(clipboard_service/paste)与落盘时用,
///   前端从头到尾没读过它,送过去只是白占 WebView 的 JS 堆(单条可达 64K 字符)。
/// - **`text` 截成预览**:列表只展示第一行、搜索也只在预览范围内;
///   完整文本仍留在后端,粘贴时前端只传 id、后端按 id 取完整记录。
#[derive(Serialize, Clone)]
pub struct RecordView {
    pub id: u64,
    pub kind: String,
    pub text: Option<String>,
    pub files: Option<Vec<String>>,
    pub image_path: Option<String>,
    pub hash: String,
    pub created_at: u64,
    pub source_app: Option<String>,
    pub group: Option<String>,
    pub favorite: bool,
}

/// 发给前端的 text 预览上限(字符)。远大于列表标题所需,又足以覆盖搜索;
/// 完整文本不丢,只是不再进 WebView。
const PREVIEW_TEXT_CHARS: usize = 4_000;

fn preview_text(text: &Option<String>) -> Option<String> {
    text.as_ref().map(|t| {
        if t.chars().count() > PREVIEW_TEXT_CHARS {
            t.chars().take(PREVIEW_TEXT_CHARS).collect()
        } else {
            t.clone()
        }
    })
}

impl From<&ClipboardRecord> for RecordView {
    fn from(rec: &ClipboardRecord) -> Self {
        Self {
            id: rec.id,
            kind: rec.kind.clone(),
            text: preview_text(&rec.text),
            files: rec.files.clone(),
            image_path: rec.image_path.clone(),
            hash: rec.hash.clone(),
            created_at: rec.created_at,
            source_app: rec.source_app.clone(),
            group: rec.group.clone(),
            favorite: rec.favorite,
        }
    }
}

static HISTORY: Lazy<Mutex<VecDeque<ClipboardRecord>>> =
    Lazy::new(|| Mutex::new(VecDeque::new()));
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SUPPRESS_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// 每次"有改动"自增(见 `mark_dirty`)。
///
/// 用**计数**而不是布尔:布尔只有一位,"有待落盘的改动"和"上次失败还没写成功"
/// 会是同一个状态,失败时只能把标志位置回去,于是磁盘满/只读时每 500ms 空转重试
/// 一次全量序列化。拆成"当前版本 / 已落盘版本"后,这两个状态天然可区分。
static CHANGE_SEQ: AtomicU64 = AtomicU64::new(0);
/// 已经成功落盘的版本(由 `save()` 在写成功后推进)
static SAVED_SEQ: AtomicU64 = AtomicU64::new(0);
/// 落盘互斥:退出前的收尾落盘可能与后台线程的落盘撞上——两者写的是同一个
/// 临时文件(`history.json.tmp`),不串行化就可能互相写坏
static SAVE_LOCK: Mutex<()> = Mutex::new(());
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
/// 单条文字字段的最大字符数(纯文本超长截断,截断只丢尾部不会坏)。
pub const MAX_TEXT_CHARS: usize = 200_000;
/// 富文本 html 的绝对上限(字符数):超过就整个不存,只保留纯文本。
/// 正常带格式内容(Word/WPS 一整页带样式)在几 KB 量级,64K 字符相当于几十页;
/// 再大基本都是 Office 粘贴带出来的垃圾样式/隐藏数据,留着写剪贴板会明显卡顿。
pub const MAX_HTML_CHARS: usize = 64_000;
/// 相对膨胀规则:html 字符数超过纯文本的 30 倍、且多于 16K 时,
/// 同样视为"样式垃圾"退化为纯文本(防"内容一点点、html 一大堆")。
const HTML_BLOAT_RATIO: usize = 30;
const HTML_BLOAT_MIN: usize = 16_000;

/// 一条富文本记录是否保留 html:false 时只存纯文本(粘贴永远可用,不会卡)。
/// html **不做拦腰截断**——坏标记会让 WPS/Word 解析失败而粘不出来,所以要么全留要么全丢。
pub fn keep_html(html: &str, text: &str) -> bool {
    let n = html.chars().count();
    if n > MAX_HTML_CHARS {
        return false;
    }
    let t = text.chars().count();
    if t >= 20 && n > HTML_BLOAT_MIN && n > t * HTML_BLOAT_RATIO {
        return false;
    }
    true
}
/// 分组名最大字符数(超长截断,避免前端被撑爆)
const MAX_GROUP_CHARS: usize = 32;
const HISTORY_FILE: &str = "history.json";
const PERSIST_VERSION: u32 = 1;
/// 变更后延迟多久落盘(连续复制只写一次)
const SAVE_DEBOUNCE_MS: u64 = 500;
/// 落盘连续失败后的最大退避间隔
const SAVE_RETRY_MAX_MS: u64 = 30_000;

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
    // 载入时可能裁剪过记录、把坏 html 修成了纯文本:把归一化后的结果固化下来
    mark_dirty();

    std::thread::spawn(|| {
        // 连续失败次数,用于退避;成功或无事可做时归零
        let mut failures: u32 = 0;
        loop {
            // 正常按 500ms 去抖;上次失败则指数退避 1s、2s、4s…上限 30s,
            // 免得磁盘满/只读时每 500ms 都白做一次全量序列化
            let wait = if failures == 0 {
                SAVE_DEBOUNCE_MS
            } else {
                (SAVE_DEBOUNCE_MS << failures.min(6)).min(SAVE_RETRY_MAX_MS)
            };
            std::thread::sleep(std::time::Duration::from_millis(wait));

            if !has_pending() {
                failures = 0;
                continue;
            }
            match save() {
                Ok(()) => failures = 0,
                Err(e) => {
                    failures = failures.saturating_add(1);
                    eprintln!("[持久化] 写入历史失败(第 {failures} 次,{wait}ms 后重试): {e}");
                }
            }
        }
    });
}

pub fn history_path() -> Option<PathBuf> {
    DATA_DIR.lock().as_ref().map(|dir| dir.join(HISTORY_FILE))
}

/// 标记"有改动待落盘"(只推版本号,不做任何 I/O)
fn mark_dirty() {
    CHANGE_SEQ.fetch_add(1, Ordering::SeqCst);
}

/// 有没有"改了但还没落盘"的内容
fn has_pending() -> bool {
    CHANGE_SEQ.load(Ordering::SeqCst) != SAVED_SEQ.load(Ordering::SeqCst)
}

/// 退出前把待落盘的改动同步写掉。
///
/// 落盘是 500ms 去抖的,`app.exit(0)` 之前不补这一次的话,最后这段窗口里的改动会丢
/// (刚复制完就退出、刚点了收藏就退出都会命中)。调用方是"所有能让进程退出的路径"。
pub fn flush_pending() {
    if !has_pending() {
        return;
    }
    match save() {
        Ok(()) => println!("[持久化] 退出前已把待落盘的改动写入"),
        Err(e) => eprintln!("[持久化] 退出前落盘失败,本次改动会丢: {e}"),
    }
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
            let mut repaired = 0usize;
            for mut rec in file.items {
                if drop_broken_html(&mut rec) {
                    repaired += 1;
                }
                hist.push_back(rec);
            }
            if repaired > 0 {
                eprintln!("[持久化] 修复 {repaired} 条被截断的富文本记录(已退化为纯文本)");
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
    // 后台线程与"退出前收尾落盘"可能同时进来,必须串行(见 SAVE_LOCK)
    let _guard = SAVE_LOCK.lock();

    // 版本要在取快照**之前**读:这样"没进快照的改动"版本一定比它大,
    // 下一轮必然还会再写一次,不会漏;反过来读就可能把没写进去的改动记成已落盘
    let version = CHANGE_SEQ.load(Ordering::SeqCst);

    let Some(path) = history_path() else {
        // 没有数据目录(定位失败):无处可写,把版本推进掉,别让线程空转
        SAVED_SEQ.store(version, Ordering::SeqCst);
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

    SAVED_SEQ.store(version, Ordering::SeqCst);
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

/// 选出该被"保留上限"淘汰的记录下标:只挑临时记录,从最旧的一端开始。
///
/// 与 `trim` 分开是为了能单测:`trim` 还要删磁盘文件,依赖全局数据目录。
/// 返回的下标**从大到小**(从队尾往前扫),调用方按这个顺序删不会串位。
fn eviction_indices(hist: &VecDeque<ClipboardRecord>, limit: usize) -> Vec<usize> {
    let transient = hist.iter().filter(|rec| is_transient(rec)).count();
    let mut excess = transient.saturating_sub(limit);
    let mut indices = Vec::new();
    let mut index = hist.len();
    while excess > 0 && index > 0 {
        index -= 1;
        if is_transient(&hist[index]) {
            indices.push(index);
            excess -= 1;
        }
    }
    indices
}

/// 按"临时记录不超过 limit 条"裁剪:只从最旧的一端淘汰临时记录,
/// 收藏/分组过的记录一律保留(与界面上"清理临时记录"的规则保持一致)。
///
/// 被淘汰的记录会**连同它在数据目录里的图片与缩略图一起删掉**——只从内存里
/// 淘汰而文件留在盘上,长期使用下就是"记录越淘汰、磁盘越涨"。
fn trim(hist: &mut VecDeque<ClipboardRecord>, limit: usize) {
    let indices = eviction_indices(hist, limit);
    if indices.is_empty() {
        return;
    }
    let mut evicted = Vec::with_capacity(indices.len());
    for index in indices {
        if let Some(rec) = hist.remove(index) {
            evicted.push(rec);
        }
    }

    // 还留在历史里的图片路径:这些文件不能删(同一张图只会有一条记录,这里再兜一层)
    let alive: Vec<String> = hist.iter().filter_map(|r| r.image_path.clone()).collect();
    for rec in &evicted {
        remove_record_files(rec, &alive);
    }
}

/// 新增(或去重置顶)一条记录并通知前端
pub fn push_and_emit(app: &AppHandle, rec: ClipboardRecord) {
    let rec = insert_record(&mut HISTORY.lock(), rec);
    mark_dirty();
    let _ = app.emit_all("clipboard-updated", RecordView::from(&rec));
}

/// 全部记录的前端视图(在锁内直接映射,不再整条克隆一遍)
pub fn get_all_views() -> Vec<RecordView> {
    HISTORY.lock().iter().map(RecordView::from).collect()
}

pub fn get_by_id(id: u64) -> Option<ClipboardRecord> {
    HISTORY.lock().iter().find(|r| r.id == id).cloned()
}

/// 设置(或取消)收藏。收藏后不占"保留条数"上限,也不会被自动淘汰。
pub fn set_favorite(id: u64, favorite: bool) -> Option<ClipboardRecord> {
    let mut hist = HISTORY.lock();
    let rec = hist.iter_mut().find(|r| r.id == id)?;
    if rec.favorite == favorite {
        // 值没变:不置脏。否则每次"重复点收藏"都会白触发一次全量重写
        let same = rec.clone();
        drop(hist);
        return Some(same);
    }
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
    if rec.group == group {
        // 值没变(比如在分组弹层里直接点确认):不置脏,免得白重写一遍 history.json
        let same = rec.clone();
        drop(hist);
        return Some(same);
    }
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

/// 历史文件里"被拦腰截断的富文本"是坏标记(老版本把 html 截到 1 万字符),
/// 粘贴进 WPS/Word 会解析失败。识别:正常 HTML 片段去尾部空白后必以 `>` 结束。
/// 命中则退化为纯文本记录(kind 同步改回 text),保证一定能粘。返回是否做了修复。
fn drop_broken_html(rec: &mut ClipboardRecord) -> bool {
    let text = rec.text.as_deref().unwrap_or_default();
    let broken = rec
        .html
        .as_deref()
        .map(|h| !h.trim_end().ends_with('>') || !keep_html(h, text))
        .unwrap_or(false);
    if broken {
        rec.html = None;
        if rec.kind == "html" {
            rec.kind = "text".into();
        }
    }
    broken
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
pub fn cmd_get_history() -> Vec<RecordView> {
    get_all_views()
}

/// 收藏 / 取消收藏一条记录
#[tauri::command]
pub fn cmd_set_favorite(id: u64, favorite: bool) -> Result<RecordView, String> {
    set_favorite(id, favorite)
        .as_ref()
        .map(RecordView::from)
        .ok_or_else(|| "记录不存在".to_string())
}

/// 把前端旧版存在 localStorage 里的收藏导入到记录上(启动时调一次)
#[tauri::command]
pub fn cmd_import_favorites(hashes: Vec<String>) -> usize {
    import_favorites(&hashes)
}

/// 给一条记录指定分组(传 null/空串 = 取消分组)
#[tauri::command]
pub fn cmd_set_group(id: u64, group: Option<String>) -> Result<RecordView, String> {
    set_group(id, group)
        .as_ref()
        .map(RecordView::from)
        .ok_or_else(|| "记录不存在".to_string())
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

    /// 全局可变状态(`HISTORY` / `DATA_DIR` / 落盘版本号)是进程级共享的。会改它们的
    /// 用例必须彼此串行,否则会出现:A 用例设的 `DATA_DIR` 被 B 用例换掉;或者 B 用例
    /// 的置脏动作让 A 用例"值没变就不该置脏"的断言假失败。
    /// 只有"会改全局状态"的用例拿这把锁,其余用例照旧并行跑。
    /// (parking_lot 的 Mutex 不中毒,某个用例 panic 不会连累其它用例)
    static GLOBAL_STATE_LOCK: Mutex<()> = Mutex::new(());

    /// 同一份内容重新复制:分组与收藏都跟着内容走,不会因为去重置顶而丢
    #[test]
    fn dedupe_inherits_group_and_favorite() {
        let _s = GLOBAL_STATE_LOCK.lock(); // 会置脏,见该锁的说明
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

    /// 淘汰选择:只挑临时记录、从最旧的一端开始(trim 删磁盘文件直接复用这个结果);
    /// 返回的下标必须互不重叠,且按从大到小给出——调用方按序 remove 才不会串位。
    #[test]
    fn eviction_picks_oldest_transient_only() {
        let mut hist = VecDeque::new();
        let oldest = insert_record(&mut hist, rec("h-ev1", "1"));
        let fav = insert_record(&mut hist, rec("h-ev2", "2"));
        let grp = insert_record(&mut hist, rec("h-ev3", "3"));
        let newest = insert_record(&mut hist, rec("h-ev4", "4"));
        for r in hist.iter_mut() {
            if r.id == fav.id {
                r.favorite = true;
            }
            if r.id == grp.id {
                r.group = Some("web".into());
            }
        }

        // 上限内不淘汰
        assert!(eviction_indices(&hist, 5).is_empty());
        assert!(eviction_indices(&hist, 2).is_empty(), "刚好 2 条临时记录");

        // 上限 1 → 只淘汰最旧的临时记录,收藏/分组的不动
        let indices = eviction_indices(&hist, 1);
        assert_eq!(indices.len(), 1);
        assert_eq!(hist[indices[0]].id, oldest.id, "淘汰最旧的临时记录");
        assert!(indices[0] < hist.len(), "下标必须可用");
        assert!(indices.windows(2).all(|w| w[0] > w[1]), "下标按从大到小给出");

        // 上限 0 → 两条临时记录都淘汰,仍不动收藏/分组
        let indices = eviction_indices(&hist, 0);
        assert_eq!(indices.len(), 2);
        let evicted: Vec<u64> = indices.iter().map(|i| hist[*i].id).collect();
        assert!(evicted.contains(&oldest.id) && evicted.contains(&newest.id));
        assert!(!evicted.contains(&fav.id) && !evicted.contains(&grp.id));
    }

    /// trim 淘汰记录时会把它们带走,并且裁掉的文件不会牵连还留着的记录
    /// (数据目录未初始化时不碰磁盘,只验证内存结果)
    #[test]
    fn trim_removes_evicted_from_history() {
        let mut hist = VecDeque::new();
        let a = insert_record(&mut hist, rec("h-tm1", "a"));
        let b = insert_record(&mut hist, rec("h-tm2", "b"));
        let c = insert_record(&mut hist, rec("h-tm3", "c"));

        trim(&mut hist, 2);

        let left: Vec<u64> = hist.iter().map(|r| r.id).collect();
        assert_eq!(left.len(), 2);
        assert!(!left.contains(&a.id), "最旧的被淘汰");
        assert!(left.contains(&b.id) && left.contains(&c.id));
    }

    /// 真正落盘的验证:淘汰时图片与缩略图必须从数据目录删掉,
    /// 而仍留在历史里的记录(收藏保护)的文件绝不能碰。
    #[test]
    fn trim_deletes_evicted_files_only() {
        let _env = GLOBAL_STATE_LOCK.lock(); // 要改全局 DATA_DIR,与同类用例串行
        let dir = std::env::temp_dir().join(format!("kp-trim-{}", std::process::id()));
        let images = dir.join("clipboard_images");
        let thumbs = images.join("thumbs");
        std::fs::create_dir_all(&thumbs).expect("建测试目录");

        // 会被淘汰的临时记录:图片 + 缩略图
        let gone_hash = "0123456789abcdef".to_string() + &"a".repeat(48);
        let gone_rel = "clipboard_images/0123456789abcdef.png".to_string();
        let gone_img = dir.join(&gone_rel);
        let gone_thumb = thumbs.join("0123456789abcdef.png");
        std::fs::write(&gone_img, b"fake").unwrap();
        std::fs::write(&gone_thumb, b"fake").unwrap();
        let mut evict = rec("x", "会被淘汰");
        evict.hash = gone_hash;
        evict.image_path = Some(gone_rel);

        // 收藏保护、不会被淘汰的记录:它的文件必须留下
        let keep_hash = "ffffffffffffffff".to_string() + &"b".repeat(48);
        let keep_rel = "clipboard_images/ffffffffffffffff.png".to_string();
        let keep_img = dir.join(&keep_rel);
        let keep_thumb = thumbs.join("ffffffffffffffff.png");
        std::fs::write(&keep_img, b"fake").unwrap();
        std::fs::write(&keep_thumb, b"fake").unwrap();
        let mut keep = rec("y", "会被保留");
        keep.hash = keep_hash;
        keep.image_path = Some(keep_rel);
        keep.favorite = true;

        let mut hist = VecDeque::new();
        hist.push_back(keep.clone()); // 旧
        hist.push_back(evict); // 新

        *DATA_DIR.lock() = Some(dir.clone());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            trim(&mut hist, 0); // 上限 0 → 临时记录全淘汰
        }));
        *DATA_DIR.lock() = None; // 立刻复位,别影响并行跑的其它用例

        assert!(result.is_ok(), "trim 不应 panic");
        assert!(!gone_img.exists(), "被淘汰记录的图片必须删掉");
        assert!(!gone_thumb.exists(), "被淘汰记录的缩略图必须删掉");
        assert!(keep_img.exists(), "仍在历史里的图片不能被删");
        assert!(keep_thumb.exists(), "仍在历史里的缩略图不能被删");
        assert_eq!(hist.iter().map(|r| r.id).collect::<Vec<_>>(), vec![keep.id]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 分组名:去空白、超长截断、空白等于取消分组
    #[test]
    fn set_group_normalizes() {
        let _s = GLOBAL_STATE_LOCK.lock(); // 会置脏,见该锁的说明
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

    /// 载入时清理被截断的坏 html:退化为纯文本;完好的 html 不动
    #[test]
    fn broken_html_is_dropped_on_load() {
        let mut bad = rec("h-html-bad", "http://a.b");
        bad.kind = "html".into();
        bad.html = Some("<html xmlns:w=\"urn:schemas\"><p>foo".into());
        assert!(drop_broken_html(&mut bad));
        assert_eq!(bad.kind, "text");
        assert!(bad.html.is_none());

        let mut good = rec("h-html-ok", "http://a.b");
        good.kind = "html".into();
        good.html = Some("<p>ok</p>\n".into());
        assert!(!drop_broken_html(&mut good));
        assert!(good.html.is_some());

        let mut plain = rec("h-text", "普通文字");
        assert!(!drop_broken_html(&mut plain));
    }

    /// 删除:只删给到的 id,其它记录不受影响
    #[test]
    fn remove_ids_is_selective() {
        let _s = GLOBAL_STATE_LOCK.lock(); // 会置脏,见该锁的说明
        let a = insert("h-a", "a");
        let b = insert("h-b", "b");
        let c = insert("h-c", "c");

        assert_eq!(remove_ids(&[b.id]), 1);
        let left = ids();
        assert!(left.contains(&a.id) && left.contains(&c.id));
        assert!(!left.contains(&b.id));
        assert_eq!(remove_ids(&[]), 0, "空列表不做任何事");
    }

    /// D2:收藏 / 分组的值没变时不该置脏。
    /// 否则用户在分组弹层里直接点"确认"(分组根本没改)也会白触发一次 history.json 全量重写。
    #[test]
    fn unchanged_favorite_or_group_does_not_mark_dirty() {
        let _s = GLOBAL_STATE_LOCK.lock(); // 断言期间若有别的用例置脏,这条会假失败
        let r = insert("h-d2", "x");

        set_favorite(r.id, true);
        let after = CHANGE_SEQ.load(Ordering::SeqCst);
        set_favorite(r.id, true); // 同值
        assert_eq!(CHANGE_SEQ.load(Ordering::SeqCst), after, "收藏值没变不应置脏");
        set_favorite(r.id, false); // 真变了
        assert!(CHANGE_SEQ.load(Ordering::SeqCst) > after, "收藏值真变了必须置脏");

        set_group(r.id, Some("web".into()));
        let after = CHANGE_SEQ.load(Ordering::SeqCst);
        set_group(r.id, Some("web".into())); // 同值
        assert_eq!(CHANGE_SEQ.load(Ordering::SeqCst), after, "分组没变不应置脏");
        set_group(r.id, Some("work".into())); // 真变了
        assert!(CHANGE_SEQ.load(Ordering::SeqCst) > after, "分组真变了必须置脏");
    }

    /// 退出前的收尾落盘:500ms 去抖窗口内的改动不能丢。
    /// 落盘是去抖的,直接 `exit(0)` 而不补一次 save,"刚复制完就退出"就会丢内容。
    #[test]
    fn flush_pending_writes_pending_changes() {
        let _env = GLOBAL_STATE_LOCK.lock();
        let dir = std::env::temp_dir().join(format!("kp-flush-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建测试目录");
        *DATA_DIR.lock() = Some(dir.clone());

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // 模拟"刚改过、还没到去抖点"
            insert_record(&mut HISTORY.lock(), rec("h-flush", "退出前这条必须落盘"));
            mark_dirty();
            assert!(has_pending(), "有改动时应当有待落盘内容");

            flush_pending();
            assert!(!has_pending(), "flush 之后不该再有待落盘内容");

            let raw = std::fs::read(dir.join("history.json")).expect("history.json 应已写出");
            let parsed: PersistFile = serde_json::from_slice(&raw).expect("写出的文件要能解析回来");
            assert_eq!(parsed.version, PERSIST_VERSION);
            assert!(
                parsed.items.iter().any(|r| r.hash == "h-flush"),
                "待落盘的记录必须在文件里"
            );
        }));

        *DATA_DIR.lock() = None; // 立刻复位,别影响并行跑的其它用例
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_ok(), "flush_pending 不应 panic");
    }
}
