//! 应用设置:一份 JSON 放在应用数据目录(`settings.json`)。
//!
//! 设置项很少,不做增量更新接口——界面把整份设置发回来,后端负责校验、应用副作用
//! (改热键、改保留条数)再落盘;热键注册失败时不落盘,界面拿到 Err 后回显原值。

use crate::klog;
use std::path::PathBuf;

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::state;

const FILE: &str = "settings.json";
/// 保留条数的允许区间(界面给的是 300/500/1000,这里放宽一些)
const MIN_ITEMS: usize = 100;
const MAX_ITEMS: usize = 10_000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 显示/隐藏窗口的全局热键(加速键写法,如 "Ctrl+Alt+V")
    pub hotkey: String,
    /// 保留的"临时记录"条数上限;收藏与分组的记录不计入、也不会被淘汰
    pub max_items: usize,
    /// 界面语言:zh | en
    pub language: String,
    /// 便捷粘贴:搜索框为空时按数字键 1-9 直接粘贴对应条目
    pub quick_paste: bool,
}

/// 默认热键:Ctrl+Shift+V(mac 上按习惯用 Cmd+Shift+V)
fn default_hotkey() -> String {
    if cfg!(target_os = "macos") {
        "Cmd+Shift+V".to_string()
    } else {
        "Ctrl+Shift+V".to_string()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: default_hotkey(),
            max_items: 500,
            language: "zh".to_string(),
            quick_paste: true,
        }
    }
}

static SETTINGS: Lazy<Mutex<Settings>> = Lazy::new(|| Mutex::new(Settings::default()));
static DIR: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));

/// 当前设置(克隆一份,调用方随便改)
pub fn get() -> Settings {
    SETTINGS.lock().clone()
}

/// 设置文件路径("存储"页展示用;启动前为 None)
pub fn file_path() -> Option<PathBuf> {
    DIR.lock().clone().map(|dir| dir.join(FILE))
}

/// 启动时载入:文件不存在/解析失败就用默认值,并立刻落一份,方便用户知道有这个文件
pub fn start(app: &AppHandle) {
    let Some(dir) = app.path_resolver().app_data_dir() else {
        eprintln!("[设置] 无法定位应用数据目录,本次使用默认设置");
        return;
    };
    let path = dir.join(FILE);

    match std::fs::read(&path) {
        Ok(raw) => match serde_json::from_slice::<Settings>(&raw) {
            Ok(loaded) => *SETTINGS.lock() = sanitize(loaded),
            Err(e) => eprintln!("[设置] 解析失败({e}),使用默认设置: {}", path.display()),
        },
        Err(_) => println!("[设置] 没有设置文件,使用默认设置: {}", path.display()),
    }

    *DIR.lock() = Some(dir);
    let current = get();
    state::set_max_items(current.max_items);
    save();
    println!(
        "[设置] 载入: 热键={} 保留条数={} 语言={} 便捷粘贴={}",
        current.hotkey, current.max_items, current.language, current.quick_paste
    );
}

/// 非法值一律拉回合法范围(界面发的值也要过一遍,免得手改文件把程序搞坏)
fn sanitize(mut settings: Settings) -> Settings {
    settings.hotkey = settings.hotkey.trim().to_string();
    if settings.hotkey.is_empty() {
        settings.hotkey = Settings::default().hotkey;
    }
    settings.max_items = settings.max_items.clamp(MIN_ITEMS, MAX_ITEMS);
    if settings.language != "en" {
        settings.language = "zh".to_string();
    }
    settings
}

fn save() {
    let Some(dir) = DIR.lock().clone() else {
        return;
    };
    let snapshot = SETTINGS.lock().clone();
    match serde_json::to_vec_pretty(&snapshot) {
        Ok(json) => {
            let path = dir.join(FILE);
            if let Err(e) = std::fs::write(&path, json) {
                eprintln!("[设置] 写入 {path:?} 失败: {e}");
            }
        }
        Err(e) => eprintln!("[设置] 序列化失败: {e}"),
    }
}

#[tauri::command]
pub fn cmd_get_settings() -> Settings {
    get()
}

#[tauri::command]
pub fn cmd_set_settings(app: AppHandle, settings: Settings) -> Result<Settings, String> {
    let next = sanitize(settings);
    let old = get();

    // 热键先注册:失败就直接返回错误,不落盘(界面会把旧的热键显示回去)。
    // 注意:值没变也要重试注册 —— 启动时可能因为被别的程序占用而没绑上,
    // 用户在设置里原样再"设一次"(比如把它重新录一遍)就是一次合理的重试机会。
    if next.hotkey != old.hotkey || !crate::hotkey::status().ok {
        crate::hotkey::apply(&app, &next.hotkey)?;
    }
    let max_changed = next.max_items != old.max_items;

    *SETTINGS.lock() = next.clone();
    save();
    if max_changed {
        state::set_max_items(next.max_items);
    }

    klog!(
        "[设置] 已更新: 热键={} 保留条数={} 语言={} 便捷粘贴={}",
        next.hotkey,
        next.max_items,
        next.language,
        next.quick_paste
    );
    Ok(next)
}
