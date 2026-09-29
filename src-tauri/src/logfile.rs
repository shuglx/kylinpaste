//! 诊断日志:同时写 stderr 和 `<应用数据目录>/kylinpaste.log`。
//!
//! 为什么需要它:目标机(银河麒麟)上用户通常不是从终端启动的,stderr 看不到;
//! 这个文件让"热键没抓上、粘贴链路走到哪一步、panic 在哪"都能事后翻出来。
//!
//! 不带额外依赖(没有 chrono):时间戳用"应用启动后的相对秒数 + unix 毫秒",
//! 排查时够用——用户描述"我按了热键"的时刻与日志里的相对时间能对上。
//!
//! **日志文件不会无限增长**:超过上限就把**最近的尾部**保留下来(见 `trim_to_tail`),
//! 而不是清空——出问题时最有价值的恰是最近那段现场。本应用可能连续运行几个月,
//! 所以除了大小闸门,真正做文件整理还要求"距上次整理够久",避免频繁读写同一个文件。

use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

const LOG_FILE: &str = "kylinpaste.log";
/// 日志文件上限:超过就整理一次(只保留 `KEEP_TAIL_BYTES` 的尾部)
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
/// 整理后保留的尾部大小。
///
/// 取上限的一半,是为了让"两次整理之间"大约隔着 1MB 的日志量:正常使用(一天几万行、
/// 每天 1~2MB)也就是一天一两次,不会刚整理完就又要整理。
const KEEP_TAIL_BYTES: u64 = 1024 * 1024;
/// 累计新写入这么多字节才去 `stat` 一次文件大小(不必每写一行都问一次内核)
const SIZE_CHECK_EVERY_BYTES: u64 = 128 * 1024;
/// 两次整理之间的最小间隔:防止"日志风暴"(某个循环疯狂打日志)时反复读写同一个文件
const MIN_ROTATE_INTERVAL_MS: u64 = 10 * 60 * 1000;

static SINK: Mutex<Option<File>> = Mutex::new(None);
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static START_MS: AtomicU64 = AtomicU64::new(0);
/// 自上次检查以来累计写入的字节数
static BYTES_SINCE_CHECK: AtomicU64 = AtomicU64::new(0);
/// 上次整理的时刻(unix 毫秒)。用墙上时钟:万一时钟被往回拨,最坏只是整理晚一点
static LAST_ROTATE_MS: AtomicU64 = AtomicU64::new(0);

/// 初始化:重定向到 `<dir>/kylinpaste.log`(启动时调用一次)
pub fn init(dir: PathBuf) {
    START_MS.store(now_ms(), Ordering::SeqCst);
    let path = dir.join(LOG_FILE);
    let _ = std::fs::create_dir_all(&dir);

    // 上一次运行的日志可能已经超限(比如进程被强杀,没赶上退出前的整理):
    // 启动时按同一套规则收一次,而不是清空——保留最近的现场更有诊断价值
    match trim_to_tail(&path) {
        Ok(true) => eprintln!("[日志] 上次的日志超过上限,已只保留最近一段: {}", path.display()),
        Ok(false) => {}
        Err(e) => eprintln!("[日志] 启动时整理日志失败(继续追加): {e}"),
    }
    LAST_ROTATE_MS.store(now_ms(), Ordering::SeqCst);

    *SINK.lock() = open_append(&path);
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

    // 先把行写进去(顺路做整理,所以整段都握着 SINK 锁)
    let mut sink = SINK.lock();
    if let Some(file) = sink.as_mut() {
        let _ = writeln!(file, "{line}");
        // 每行 flush 只把数据交给内核(不做 fsync):进程崩溃也能保住最后一行,
        // 而代价只是每行一次系统调用——这点开销换"崩溃现场"很值
        let _ = file.flush();
    }

    // 日志别无限长:检查按写入量节流(不是每行都 stat),真正整理还要求超限且间隔够
    if BYTES_SINCE_CHECK.fetch_add(line.len() as u64 + 1, Ordering::SeqCst)
        >= SIZE_CHECK_EVERY_BYTES
    {
        BYTES_SINCE_CHECK.store(0, Ordering::SeqCst);
        rotate_if_needed(&mut sink);
    }
}

/// 超过上限就整理成"只保留尾部"。调用方**必须持有 SINK 锁**:整理会把文件换掉,
/// 期间不能让别的写入落在即将失效的旧句柄上。
fn rotate_if_needed(sink: &mut Option<File>) {
    let Some(path) = LOG_PATH.lock().clone() else {
        return;
    };
    let Ok(size) = std::fs::metadata(&path).map(|m| m.len()) else {
        return;
    };
    if !should_rotate(size, now_ms(), LAST_ROTATE_MS.load(Ordering::SeqCst)) {
        return;
    }

    // 先放下写句柄:文件马上要被替换掉,句柄会继续指向被删掉的旧 inode
    *sink = None;
    match trim_to_tail(&path) {
        Ok(_) => {
            LAST_ROTATE_MS.store(now_ms(), Ordering::SeqCst);
            *sink = open_append(&path);
        }
        Err(e) => {
            // 整理失败也要把句柄接回去继续写:不能因为整理失败就丢日志
            *sink = open_append(&path);
            eprintln!("[日志] 整理失败(继续追加): {e}");
        }
    }
}

/// 是否该整理:超限,且距上次整理已过最小间隔(纯判断,便于单测)
fn should_rotate(size: u64, now: u64, last_rotate_ms: u64) -> bool {
    size > MAX_LOG_BYTES && now.saturating_sub(last_rotate_ms) >= MIN_ROTATE_INTERVAL_MS
}

fn open_append(path: &Path) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// 只保留文件末尾 `KEEP_TAIL_BYTES` 的内容(从行边界开始,不留下半行)。
///
/// 文件没超限或不存在时什么都不做,返回 `false`。整理走"临时文件 + 改名",
/// 与历史落盘同一套写法:写一半被打断也不会把日志文件弄坏。
fn trim_to_tail(path: &Path) -> std::io::Result<bool> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    if len <= KEEP_TAIL_BYTES {
        return Ok(false);
    }

    file.seek(SeekFrom::Start(len - KEEP_TAIL_BYTES))?;
    let mut bytes = Vec::with_capacity(KEEP_TAIL_BYTES as usize + 1);
    file.read_to_end(&mut bytes)?;
    drop(file);

    // 进程被强杀时可能留下半个字符,所以按"可能非法"的字节去解,不让它把整理搞失败
    let text = String::from_utf8_lossy(&bytes);
    let tail = match text.find('\n') {
        Some(i) => &text[i + 1..], // 丢掉开头那半行
        None => text.as_ref(),
    };

    let tmp = path.with_extension("log.tmp");
    let mut out = File::create(&tmp)?;
    writeln!(
        out,
        "{} ---- 日志超过 {} KB,以下只保留最近的部分 ----",
        stamp(),
        MAX_LOG_BYTES / 1024
    )?;
    out.write_all(tail.as_bytes())?;
    out.flush()?;
    drop(out);
    std::fs::rename(&tmp, path)?;
    Ok(true)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个用例用各自的临时目录(按名字区分,免得并行跑时互相踩)
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kp-log-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建测试目录");
        dir
    }

    /// 一行 60 字节的定长内容,便于断言"保留的是完整行"
    fn line(i: usize) -> String {
        format!("{i:0>60}\n")
    }

    /// 整理只保留**尾部**:最早的被丢掉、最近的完整保留,且不留下半行
    #[test]
    fn trim_keeps_only_the_tail() {
        let dir = temp_dir("trim");
        let path = dir.join(LOG_FILE);

        // 造一个明显超限的文件(3 倍保留量)
        let total = (3 * KEEP_TAIL_BYTES as usize) / 61;
        let mut content = String::new();
        for i in 0..total {
            content.push_str(&line(i));
        }
        std::fs::write(&path, &content).expect("写测试日志");
        assert!(content.len() as u64 > MAX_LOG_BYTES, "测试前提:原文件超限");

        assert!(trim_to_tail(&path).expect("整理不应失败"), "超限就该整理");

        let after = std::fs::read_to_string(&path).expect("读回整理结果");
        assert!(
            after.len() as u64 <= KEEP_TAIL_BYTES + 200,
            "整理后只剩尾部,实际 {}",
            after.len()
        );
        assert!(after.contains(&line(total - 1)), "最后一行必须保留");
        assert!(!after.contains(&line(0)), "最早的内容必须被丢掉");

        // 第一行是提示行,第二行起必须都是完整行(不能从半行开始)
        let body: Vec<&str> = after.lines().skip(1).collect();
        assert!(!body.is_empty(), "整理后不该只剩提示行");
        assert_eq!(body[0].len(), 60, "保留部分必须从完整行开始");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 没超限就一动不动(不能因为"顺手整理"把日志越整理越少)
    #[test]
    fn trim_leaves_small_file_alone() {
        let dir = temp_dir("small");
        let path = dir.join(LOG_FILE);
        std::fs::write(&path, "一条短日志\n").expect("写测试日志");

        assert!(!trim_to_tail(&path).expect("整理不应失败"), "没超限不该整理");
        assert_eq!(
            std::fs::read_to_string(&path).expect("读回"),
            "一条短日志\n",
            "没超限时内容必须原样不动"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件不存在(首次启动)不该当成错误
    #[test]
    fn trim_tolerates_missing_file() {
        let dir = temp_dir("missing");
        let path = dir.join(LOG_FILE);
        assert!(!trim_to_tail(&path).expect("文件不存在不该报错"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 端到端:写满到超限时应当整理,而且**整理之后写入的内容必须还落在文件里**。
    ///
    /// 整理是"临时文件 + 改名"换掉文件,旧句柄会继续指向被删掉的旧 inode ——
    /// 忘了重新打开,之后的日志就写进了谁也看不见的文件。这一点必须守住。
    #[test]
    fn write_rotates_and_keeps_writing_to_the_new_file() {
        let dir = temp_dir("rotate");
        let path = dir.join(LOG_FILE);

        // 先塞一个略超上限的文件,让下一次检查必然触发整理
        let mut pre = String::new();
        while (pre.len() as u64) <= MAX_LOG_BYTES + SIZE_CHECK_EVERY_BYTES {
            pre.push_str(&line(pre.len() / 61));
        }
        std::fs::write(&path, &pre).expect("写测试日志");

        // 借用全局写入通道(本模块只有这一个用例会碰它),用完还原
        let mut sink_guard = SINK.lock();
        let previous_sink = sink_guard.take();
        let previous_path = LOG_PATH.lock().clone();
        *LOG_PATH.lock() = Some(path.clone());
        *sink_guard = open_append(&path);
        LAST_ROTATE_MS.store(0, Ordering::SeqCst); // 允许整理
        BYTES_SINCE_CHECK.store(SIZE_CHECK_EVERY_BYTES, Ordering::SeqCst); // 这次写就触发检查
        drop(sink_guard);

        // 第一次写:顺带触发一次整理
        write(format_args!("整理之前的这一行"));
        let trimmed = std::fs::read(&path).expect("读回日志");
        assert!(
            trimmed.len() as u64 <= KEEP_TAIL_BYTES + 4096,
            "超限应当被整理变小,实际 {} 字节",
            trimmed.len()
        );

        // 第二次写:必须落在"整理后"的那个文件里。
        // 如果整理后忘了重新打开句柄,这一行会写进被改名换掉的旧 inode,断言失败。
        write(format_args!("整理之后的这一行必须还在"));
        let after = std::fs::read(&path).expect("读回日志");
        assert!(
            String::from_utf8_lossy(&after).contains("整理之后的这一行必须还在"),
            "整理后新写的行丢了:句柄可能还指向被改名换掉的旧文件"
        );

        // 还原全局状态,别影响其它用例
        *SINK.lock() = previous_sink;
        *LOG_PATH.lock() = previous_path;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 整理条件:既要超限,又要距上次整理够久(避免日志风暴时反复读写)
    #[test]
    fn rotate_policy_needs_size_and_interval() {
        let now = 10 * MIN_ROTATE_INTERVAL_MS;
        assert!(!should_rotate(MAX_LOG_BYTES, now, 0), "没超限不整理");
        assert!(
            should_rotate(MAX_LOG_BYTES + 1, now, now - MIN_ROTATE_INTERVAL_MS),
            "超限且间隔够就整理"
        );
        assert!(
            !should_rotate(MAX_LOG_BYTES + 1, now, now - MIN_ROTATE_INTERVAL_MS + 1),
            "间隔不够不整理"
        );
        assert!(
            !should_rotate(MAX_LOG_BYTES + 1, 1_000, 900_000),
            "刚整理过不重复整理(也算时钟往回拨的情况)"
        );
    }
}
