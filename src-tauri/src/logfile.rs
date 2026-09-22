//! 诊断日志:同时写 stderr 和 `<应用数据目录>/kylinpaste.log`。
//!
//! 为什么需要它:目标机(银河麒麟)上用户通常不是从终端启动的,stderr 看不到;
//! 这个文件让"热键没抓上、粘贴链路走到哪一步、panic 在哪"都能事后翻出来。
//!
//! 不带额外依赖(没有 chrono):时间戳用"应用启动后的相对秒数 + unix 毫秒",
//! 排查时够用——用户描述"我按了热键"的时刻与日志里的相对时间能对上。

use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// 日志超过这个大小就重开(避免长期运行无限增长)
const MAX_LOG_BYTES: u64 = 512 * 1024;

static SINK: Mutex<Option<File>> = Mutex::new(None);
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static START_MS: AtomicU64 = AtomicU64::new(0);

/// 初始化:重定向到 `<dir>/kylinpaste.log`(启动时调用一次)
pub fn init(dir: PathBuf) {
    START_MS.store(now_ms(), Ordering::SeqCst);
    let path = dir.join("kylinpaste.log");
    let _ = std::fs::create_dir_all(&dir);

    let too_big = std::fs::metadata(&path)
        .map(|m| m.len() > MAX_LOG_BYTES)
        .unwrap_or(false);
    let file = if too_big {
        File::create(&path).ok()
    } else {
        OpenOptions::new().create(true).append(true).open(&path).ok()
    };
    *SINK.lock() = file;
    *LOG_PATH.lock() = Some(path.clone());
    write(format_args!("======== 启动 KylinPaste {} ========", env!("CARGO_PKG_VERSION")));
    write(format_args!("日志文件: {}", path.display()));
}

/// 日志文件路径(前端/用户要看时用)
pub fn path() -> Option<PathBuf> {
    LOG_PATH.lock().clone()
}

/// 写一行日志(带时间戳)。没有初始化时只写 stderr,不影响正常流程。
pub fn write(args: Arguments) {
    let line = format!("{} {}", stamp(), args);
    eprintln!("{line}");
    if let Some(file) = SINK.lock().as_mut() {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

/// 把 panic 也写进日志:麒麟上"用着用着窗口没了"多半是 panic,但用户看不到 stderr
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 不写参数类型,让编译器自己推断(不同 rustc 版本里这个类型改过名)
        let where_ = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "(未知)".to_string());
        write(format_args!("!!!! panic: {info}"));
        write(format_args!(
            "panic 位置 {where_}(要详细堆栈请设 RUST_BACKTRACE=1 再启动)"
        ));
        default_hook(info);
    }));
}

/// `[+12.345s]`:相对启动时间比绝对时间更便于和用户操作对照
fn stamp() -> String {
    let started = START_MS.load(Ordering::SeqCst);
    if started == 0 {
        return "[+?]".to_string();
    }
    let ms = now_ms().saturating_sub(started);
    format!("[+{}.{:03}s]", ms / 1000, ms % 1000)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 打印关键环境:排查"热键抓不上/粘贴无效"第一眼看这个(X11 还是 Wayland、什么桌面)
pub fn log_environment() {
    for key in [
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_SESSION_DESKTOP",
        "LANG",
    ] {
        write(format_args!("环境 {key}={:?}", std::env::var(key).ok()));
    }
    if let Ok(release) = std::fs::read_to_string("/etc/os-release") {
        for line in release.lines().filter(|l| l.starts_with("PRETTY_NAME")).take(1) {
            write(format_args!("系统 {line}"));
        }
    }
    if let Ok(text) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        write(format_args!("内核 {}", text.trim()));
    }
    // Wayland 会话下 X11 的被动抓键只对 X 客户端生效,全局热键会"看着注册成功但不触发"
    let wayland = std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false)
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false);
    if wayland {
        write(format_args!(
            "警告:检测到 Wayland 会话 —— X11 全局热键对原生 Wayland 窗口不会触发(粘贴注入同理)"
        ));
    }
}

#[macro_export]
macro_rules! klog {
    ($($arg:tt)*) => { $crate::logfile::write(format_args!($($arg)*)) };
}
