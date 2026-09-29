//! Linux(X11)平台底层能力:全局热键、焦点归还、按键注入。
//!
//! 为什么要自己写而不用内置能力(目标机实测结论,勿轻易改回):
//!
//! 1. Tauri v1 的全局热键在 Linux 上的实现来自 tao:它在按键**释放**时才判定命中,
//!    并用"释放那一刻"的修饰键状态去查表。用户先松开 Ctrl/Alt 再松开 V 时,
//!    状态里已经不含修饰键,热键就匹配不上——表现为"按好几下才召唤得出,有时怎么按都没反应";
//!    另外它只注册 NumLock/CapsLock 两种锁定修饰组合,系统开着 ScrollLock 时同样失效。
//!    这里改为 XGrabKey + 监听 KeyPress,并按当前键盘映射动态覆盖全部锁定修饰键组合。
//!
//! 2. enigo 注入按键前要用 XInput 枚举输入设备,并用 xkbcommon 按布局查键码;
//!    这里直接用键盘映射把 V / Control_L 解析成键码再用 XTEST 注入,链路更短更可控。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use parking_lot::Mutex;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt, EventMask,
    GrabMode, InputFocus, ModMask, Window, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
};
use x11rb::protocol::xtest;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::CURRENT_TIME;

type XResult<T> = Result<T, String>;

// 用于识别"锁定类"修饰键的 keysym
const KEYSYM_CAPSLOCK: u32 = 0xffe5;
const KEYSYM_NUMLOCK: u32 = 0xff7f;
const KEYSYM_SCROLLLOCK: u32 = 0xff14;
/// XK_Control_L
const KEYSYM_CONTROL_L: u32 = 0xffe3;

/// 唤起剪贴板之前桌面上处于活动状态的那个窗口,粘贴时把焦点还给它
static TARGET_WINDOW: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------- 连接与查询

fn connect() -> XResult<(RustConnection, Window)> {
    let (conn, screen_num) =
        RustConnection::connect(None).map_err(|e| format!("连接 X11 失败: {e}"))?;
    let root = conn.setup().roots[screen_num].root;
    Ok((conn, root))
}

/// 复用的 X11 连接(连同它对应的 root 窗口)。
///
/// 以前每个入口都自己 `connect()`:一次成功握手 + 一条新 socket。而"每次复制"
/// (取来源应用)、"每次热键"、"每次粘贴"(归还焦点 / 注入按键)都要调好几次,
/// 这些握手纯属浪费。`RustConnection` 本身就是按多线程使用设计的(内部自带锁,
/// 且有明确的加锁顺序),所以共享一条即可,**不必再套一把自己的锁**把往返串行化——
/// `activate_window` 最长要轮询 400ms,串起来会直接拖慢粘贴链路。
///
/// 连接出错(例如 X 会话重启)时丢掉缓存,下一次调用自动重连。
/// 注意:热键监听线程有**自己**的一条连接,不能共用——被动抓键归属发起它的连接。
static SHARED_CONN: Lazy<Mutex<Option<Arc<SharedConn>>>> = Lazy::new(|| Mutex::new(None));

struct SharedConn {
    conn: RustConnection,
    root: Window,
}

/// 取一条可复用的连接。拿到的是 `Arc`,**调用期间不要持有 `SHARED_CONN` 的锁**
/// (往返可能上百毫秒,持锁会挡住别的线程)。
fn shared_conn() -> XResult<Arc<SharedConn>> {
    let mut guard = SHARED_CONN.lock();
    if guard.is_none() {
        let (conn, root) = connect()?;
        *guard = Some(Arc::new(SharedConn { conn, root }));
    }
    Ok(Arc::clone(guard.as_ref().expect("刚建过")))
}

/// 连接可能已经坏了:丢掉缓存,让下一次调用重新连
fn forget_shared_conn() {
    *SHARED_CONN.lock() = None;
}

/// 在共享连接上做一次操作;出错即认为连接可能已坏,丢弃缓存(下一次调用重连)
fn with_shared<T>(f: impl FnOnce(&RustConnection, Window) -> XResult<T>) -> XResult<T> {
    let shared = shared_conn()?;
    match f(&shared.conn, shared.root) {
        Ok(value) => Ok(value),
        Err(e) => {
            forget_shared_conn();
            Err(e)
        }
    }
}

fn intern_atom(conn: &RustConnection, name: &[u8]) -> XResult<Atom> {
    conn.intern_atom(false, name)
        .map_err(|e| format!("X11 请求失败: {e}"))?
        .reply()
        .map_err(|e| format!("X11 响应失败: {e}"))
        .map(|reply| reply.atom)
}

/// 读取根窗口上的 _NET_ACTIVE_WINDOW(EWMH 约定的"当前活动窗口")
fn active_window(conn: &RustConnection, root: Window) -> Option<Window> {
    let atom = intern_atom(conn, b"_NET_ACTIVE_WINDOW").ok()?;
    conn.get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()
        .filter(|win| *win != 0)
}

/// 读取窗口的 _NET_WM_PID,用于判断某个窗口是不是本应用的
fn window_pid(conn: &RustConnection, win: Window) -> Option<u32> {
    let atom = intern_atom(conn, b"_NET_WM_PID").ok()?;
    conn.get_property(false, win, atom, AtomEnum::CARDINAL, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()
}

/// 读取窗口的 WM_CLASS(class 部分),用它当"来源应用"标识。
///
/// WM_CLASS 是一串 `实例\0类\0`,类名更规范(如 "firefox"、"Wps"、"XTerm"),
/// 所以优先取第二个;取不到就退回第一个。窗口没有 WM_CLASS 时返回 None。
fn window_class(conn: &RustConnection, win: Window) -> Option<String> {
    let reply = conn
        .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
        .ok()?
        .reply()
        .ok()?;
    let parts: Vec<String> = reply
        .value
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    parts.last().or_else(|| parts.first()).cloned()
}

/// 当前活动窗口所属应用的标识(用于记录"这条剪贴板内容来自哪个程序")。
///
/// 剪贴板变化事件通常紧跟复制动作发生,此时源应用还是活动窗口。
/// 活动窗口是本应用自己时不记(避免把"我们自己写回剪贴板"当成来源)。
pub fn active_app_name() -> Option<String> {
    let shared = shared_conn().ok()?;
    let win = active_window(&shared.conn, shared.root)?;
    if window_pid(&shared.conn, win) == Some(std::process::id()) {
        return None;
    }
    window_class(&shared.conn, win)
}

/// 焦点是否已经离开本应用(说明窗口管理器正常地把焦点还给了别的窗口)。
///
/// 返回 false 有两种情况:焦点还在自己身上,或者根本没有活动窗口——
/// 两者都意味着"需要手动归还焦点",否则模拟按键不知道往哪送。
pub fn focus_left_app() -> bool {
    let Ok(shared) = shared_conn() else {
        return false;
    };
    match active_window(&shared.conn, shared.root) {
        Some(win) => window_pid(&shared.conn, win) != Some(std::process::id()),
        None => false,
    }
}

/// 读取根窗口的 _NET_CLIENT_LIST(EWMH 维护的"受管理窗口列表")
fn client_list(conn: &RustConnection, root: Window) -> Vec<Window> {
    let Ok(atom) = intern_atom(conn, b"_NET_CLIENT_LIST") else {
        return Vec::new();
    };
    let Some(reply) = conn
        .get_property(false, root, atom, AtomEnum::WINDOW, 0, 256)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return Vec::new();
    };
    reply.value32().map(|it| it.collect()).unwrap_or_default()
}

/// 找本应用主窗口的 X11 窗口 id。
///
/// 呼出/置顶都要走 EWMH:UKUI 这类桌面对 GTK 层的 set_focus / keep_above
/// 常有"防抢焦点"式的忽略,窗口明明在跑就是呼不到最前、置顶也不生效。
/// 按 `_NET_WM_PID + WM_CLASS` 匹配;class 读不到时退回只比 pid。
pub fn main_window_xid() -> Option<Window> {
    let shared = shared_conn().ok()?;
    find_main_window(&shared.conn, shared.root)
}

/// 在"受管理窗口列表"里找本应用的主窗口(按 `_NET_WM_PID + WM_CLASS` 匹配)
fn find_main_window(conn: &RustConnection, root: Window) -> Option<Window> {
    let me = std::process::id();
    client_list(conn, root).into_iter().find(|&win| {
        window_pid(conn, win) == Some(me)
            && window_class(conn, win)
                .map(|class| class.to_ascii_lowercase().contains("kylinpaste"))
                .unwrap_or(true)
    })
}

/// 本应用主窗口当前是否就是"活动窗口"。
///
/// 直接问 X11,不依赖 GTK 的 Focused 事件 —— 镜像状态在部分桌面上收不到
/// 焦点变化事件,会让热键切换走错分支(该前置时却什么也不做)。
pub fn main_window_is_active() -> bool {
    let Ok(shared) = shared_conn() else {
        return false;
    };
    match active_window(&shared.conn, shared.root) {
        Some(win) => window_pid(&shared.conn, win) == Some(std::process::id()),
        None => false,
    }
}

/// 请求窗口管理器把主窗口设为/取消"总在最前"(EWMH `_NET_WM_STATE` 消息)。
///
/// 与 GTK 的 set_always_on_top 并行使用:GTK 在部分 WM 上不生效,自己发一遍
/// `_NET_WM_STATE_ABOVE` 最稳(与 `activate_window` 同样的 SUBSTRUCTURE_REDIRECT 约定)。
pub fn set_above(enabled: bool) -> bool {
    let Ok(shared) = shared_conn() else {
        return false;
    };
    let (conn, root) = (&shared.conn, shared.root);
    let Some(win) = find_main_window(conn, root) else {
        return false;
    };
    let (state, above) = match (
        intern_atom(conn, b"_NET_WM_STATE"),
        intern_atom(conn, b"_NET_WM_STATE_ABOVE"),
    ) {
        (Ok(state), Ok(above)) => (state, above),
        _ => {
            forget_shared_conn();
            return false;
        }
    };
    // data.l[0]: 1=添加属性 / 0=移除;l[1]=要改的属性
    let event = ClientMessageEvent::new(
        32,
        win,
        state,
        [u32::from(enabled), above, 0, 0, 0],
    );
    let sent = conn
        .send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )
        .map_err(|e| e.to_string())
        .and_then(|cookie| cookie.check().map_err(|e| e.to_string()));
    if let Err(e) = sent {
        crate::klog!("[窗口] 发送 _NET_WM_STATE_ABOVE 失败: {e}");
        forget_shared_conn();
        return false;
    }
    let _ = conn.flush();
    true
}

// ---------------------------------------------------------------- 目标窗口

/// 把"当前活动窗口"记为粘贴时的焦点归还目标。
///
/// 如果活动窗口已经是自己(例如剪贴板窗口本来就开着),不覆盖上一次的记录。
/// 建议在**抢占焦点之前**调用(热键/托盘显示窗口前、进程启动时)。
pub fn remember_target_window() {
    let Ok(shared) = shared_conn() else {
        return;
    };
    let (conn, root) = (&shared.conn, shared.root);
    let Some(win) = active_window(conn, root) else {
        return;
    };
    if window_pid(conn, win) == Some(std::process::id()) {
        return;
    }
    let old = TARGET_WINDOW.swap(win, Ordering::SeqCst);
    if old != win {
        eprintln!("[X11] 记住目标窗口: 0x{win:x}(原 0x{old:x})");
    }
}

/// 上次记录的目标窗口(用于粘贴时归还焦点)
pub fn target_window() -> Option<Window> {
    let win = TARGET_WINDOW.load(Ordering::SeqCst);
    (win != 0).then_some(win)
}

fn wait_active(conn: &RustConnection, root: Window, win: Window, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if active_window(conn, root) == Some(win) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 显式把输入焦点交给 `win`,返回是否观察到焦点确实切了过去。
///
/// 先按 EWMH 发 _NET_ACTIVE_WINDOW(source=2,表示用户主动请求,可绕过防抢焦点限制),
/// 窗口管理器不理会时再退回 XSetInputFocus。
pub fn activate_window(win: Window) -> bool {
    let shared = match shared_conn() {
        Ok(shared) => shared,
        Err(e) => {
            eprintln!("[X11] {e}");
            return false;
        }
    };
    let (conn, root) = (&shared.conn, shared.root);
    // 目标窗口可能已经被关掉(记录之后才关的),先确认它还在,避免白等一轮超时
    let alive = conn
        .get_window_attributes(win)
        .map_err(|e| e.to_string())
        .and_then(|cookie| cookie.reply().map_err(|e| e.to_string()))
        .is_ok();
    if !alive {
        eprintln!("[X11] 目标窗口 0x{win:x} 已不存在,放弃焦点归还");
        return false;
    }

    let atom = match intern_atom(conn, b"_NET_ACTIVE_WINDOW") {
        Ok(atom) => atom,
        Err(e) => {
            eprintln!("[X11] {e}");
            forget_shared_conn();
            return false;
        }
    };

    // EWMH 约定:data.l[0]=时间戳,data.l[1]=来源(2 = 分页器/用户主动请求,
    // 窗口管理器一般会绕过防抢焦点限制放行),其余字段未用
    let event = ClientMessageEvent::new(32, win, atom, [CURRENT_TIME, 2u32, 0, 0, 0]);
    let sent = conn
        .send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )
        .map_err(|e| e.to_string())
        .and_then(|cookie| cookie.check().map_err(|e| e.to_string()));
    if let Err(e) = sent {
        eprintln!("[X11] 发送 _NET_ACTIVE_WINDOW 失败: {e}");
    }

    if wait_active(conn, root, win, Duration::from_millis(400)) {
        return true;
    }

    eprintln!("[X11] 窗口管理器未响应 _NET_ACTIVE_WINDOW,回退 XSetInputFocus");
    if let Err(e) = conn
        .set_input_focus(InputFocus::POINTER_ROOT, win, CURRENT_TIME)
        .map_err(|e| e.to_string())
        .and_then(|cookie| cookie.check().map_err(|e| e.to_string()))
    {
        eprintln!("[X11] XSetInputFocus 失败: {e}");
        forget_shared_conn();
        return false;
    }
    let _ = conn.flush();
    wait_active(conn, root, win, Duration::from_millis(200))
}

// ---------------------------------------------------------------- 全局热键

/// 键盘映射里各键位的键码列表
struct KeyMap {
    min_keycode: u8,
    keysyms_per_keycode: usize,
    keysyms: Vec<u32>,
}

impl KeyMap {
    fn load(conn: &RustConnection) -> XResult<Self> {
        let setup = conn.setup();
        let min_keycode = setup.min_keycode;
        let max_keycode = setup.max_keycode;
        let reply = conn
            .get_keyboard_mapping(min_keycode, max_keycode - min_keycode + 1)
            .map_err(|e| format!("读取键盘映射失败: {e}"))?
            .reply()
            .map_err(|e| format!("读取键盘映射失败: {e}"))?;
        Ok(Self {
            min_keycode,
            keysyms_per_keycode: usize::from(reply.keysyms_per_keycode).max(1),
            keysyms: reply.keysyms,
        })
    }

    /// 取键位的第一个(未加修饰的)键码
    fn keycode_of(&self, keysym: u32) -> Option<u8> {
        self.keysyms
            .chunks(self.keysyms_per_keycode)
            .position(|chunk| chunk.first() == Some(&keysym))
            .and_then(|index| u8::try_from(usize::from(self.min_keycode) + index).ok())
    }

    /// 包含指定键码的所有键位
    fn keycodes_of_any(&self, keysyms: &[u32]) -> Vec<u8> {
        self.keysyms
            .chunks(self.keysyms_per_keycode)
            .enumerate()
            .filter(|(_, chunk)| chunk.iter().any(|sym| keysyms.contains(sym)))
            .filter_map(|(index, _)| u8::try_from(usize::from(self.min_keycode) + index).ok())
            .collect()
    }
}

/// 找出"锁定类"修饰键(CapsLock/NumLock/ScrollLock)对应的修饰键位。
///
/// XGrabKey 要求修饰键状态完全一致,所以这些锁开着时要另外注册一遍组合。
fn lock_modifier_bits(conn: &RustConnection, keymap: &KeyMap) -> XResult<u16> {
    let lock_keycodes = keymap.keycodes_of_any(&[KEYSYM_CAPSLOCK, KEYSYM_NUMLOCK, KEYSYM_SCROLLLOCK]);
    let mut bits = 0u16;
    if !lock_keycodes.is_empty() {
        let reply = conn
            .get_modifier_mapping()
            .map_err(|e| format!("读取修饰键映射失败: {e}"))?
            .reply()
            .map_err(|e| format!("读取修饰键映射失败: {e}"))?;
        let keycodes_per_modifier = reply.keycodes.len() / 8;
        if keycodes_per_modifier > 0 {
            for (index, chunk) in reply.keycodes.chunks(keycodes_per_modifier).enumerate() {
                // 跳过 Shift(0)/Control(2)/Mod1(3,Alt)/Mod4(6,Super):
                // 它们是热键本身的修饰键,不能当作"可忽略的锁定键"
                if matches!(index, 0 | 2 | 3 | 6) {
                    continue;
                }
                if chunk.iter().any(|keycode| lock_keycodes.contains(keycode)) {
                    bits |= 1 << index;
                }
            }
        }
    }
    if bits == 0 {
        // 兜底:X11 默认约定 CapsLock=Lock(1<<1)、NumLock=Mod2(1<<4)
        bits = (1 << 1) | (1 << 4);
    }
    Ok(bits)
}

/// 由位掩码展开出所有子集,用于把所有锁定键组合都注册上
fn modifier_combos(bits: u16) -> Vec<u16> {
    let mut combos = vec![0u16];
    for index in 0..16 {
        let bit = 1u16 << index;
        if bits & bit == 0 {
            continue;
        }
        let mut extended = combos.clone();
        extended.extend(combos.iter().map(|combo| combo | bit));
        combos = extended;
    }
    combos
}

/// 交给监听线程的请求(监听线程独占 X 连接,抓/放键都必须由它做)。
///
/// 结果由**请求自带**的通道回传:以前回传通道是一个全局单槽(`HOTKEY_REPLY`),
/// 并发调用会把彼此的 `Sender` 覆盖掉;请求本身也是单槽(`Option<HotkeyReq>`),
/// 后到的会把前一个还没被处理的请求直接挤掉——被挤掉的那个调用者只能白等 3 秒,
/// 然后误报"监听线程没有响应"。现在请求排队、通道跟着请求走,两处都没了。
enum HotkeyReq {
    /// 改绑(seq 仅用于日志)
    Set {
        seq: u64,
        accel: String,
        reply: Sender<XResult<()>>,
    },
    /// 暂停:放开当前抓住的组合。
    ///
    /// 设置界面录快捷键时必须先放开——被动抓键(owner_events=false)会把该组合的
    /// 主键事件投递给抓键方(根窗口),webview 根本收不到,于是"录不进当前热键"。
    Pause { reply: Sender<XResult<()>> },
    /// 恢复:按当前加速键重新抓
    Resume { reply: Sender<XResult<()>> },
}

/// 待处理的请求队列:监听线程轮询到就依次执行
static HOTKEY_REQ: Lazy<Mutex<VecDeque<HotkeyReq>>> = Lazy::new(|| Mutex::new(VecDeque::new()));
static HOTKEY_SEQ: AtomicU64 = AtomicU64::new(0);
/// 当前生效的加速键(暂停后恢复时要用)
static HOTKEY_ACCEL: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

/// 主键名 → X11 keysym(小写字母、数字的 keysym 就等于 ASCII 码)
fn keysym_of(token: &str) -> Option<u32> {
    let lower = token.to_ascii_lowercase();
    let named = match lower.as_str() {
        "," => Some(0x2c),
        "-" => Some(0x2d),
        "." => Some(0x2e),
        "/" => Some(0x2f),
        "space" => Some(0x20),
        "=" => Some(0x3d),
        "backspace" => Some(0xff08),
        "tab" => Some(0xff09),
        "enter" | "return" => Some(0xff0d),
        "escape" | "esc" => Some(0xff1b),
        "home" => Some(0xff50),
        "left" => Some(0xff51),
        "up" => Some(0xff52),
        "right" => Some(0xff53),
        "down" => Some(0xff54),
        "pageup" => Some(0xff55),
        "pagedown" => Some(0xff56),
        "end" => Some(0xff57),
        "delete" | "del" => Some(0xffff),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    // F1..F12
    if let Some(num) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=12).contains(&num) {
            return Some(0xffbe + num - 1);
        }
    }
    let mut chars = lower.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c as u32),
        _ => None,
    }
}

/// 把加速键(如 `Ctrl+Alt+V`)解析成 (修饰键掩码, 主键 keysym)
fn parse_accel(accel: &str) -> XResult<(u16, u32)> {
    let mut mods = 0u16;
    let mut key = None;
    for token in accel.split('+') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= 1 << 2,
            "alt" | "option" | "mod1" => mods |= 1 << 3,
            "shift" => mods |= 1 << 0,
            "super" | "cmd" | "command" | "meta" | "win" => mods |= 1 << 6,
            _ => key = keysym_of(token),
        }
    }
    let keysym = key.ok_or_else(|| format!("无法识别热键里的主键: {accel}"))?;
    if mods == 0 {
        return Err(format!("热键至少要带一个修饰键: {accel}"));
    }
    Ok((mods, keysym))
}

/// 当前抓住的组合(改绑时按原样 ungrab)
struct Grab {
    keycode: u8,
    mods: u16,
    lock_combos: Vec<u16>,
}

/// 放开一个热键(连同锁定键组合各一份)
fn release(conn: &RustConnection, root: Window, grab: &Grab) {
    for extra in &grab.lock_combos {
        let _ = conn.ungrab_key(grab.keycode, root, ModMask::from(grab.mods | extra));
    }
}

/// 按加速键抓键:**先抓新的,成功后再放开旧的**,改绑失败时旧热键仍然可用。
///
/// 锁定键(CapsLock/NumLock/ScrollLock)开着的组合要各注册一份,所以一个热键
/// 实际会 grab 多个组合;某个组合被别的程序占用不影响其它组合。
fn grab_hotkey(
    conn: &RustConnection,
    root: Window,
    keymap: &KeyMap,
    accel: &str,
    previous: Option<&Grab>,
) -> XResult<Grab> {
    let (mods, keysym) = parse_accel(accel)?;
    let keycode = keymap
        .keycode_of(keysym)
        .ok_or_else(|| format!("键盘映射里找不到这个按键(keysym={keysym:#x})"))?;
    let lock_combos = modifier_combos(lock_modifier_bits(conn, keymap)?);

    let mut grabbed = 0usize;
    let mut failures = Vec::new();
    for extra in &lock_combos {
        let result = conn
            .grab_key(
                false,
                root,
                ModMask::from(mods | extra),
                keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .map_err(|e| e.to_string())
            .and_then(|cookie| cookie.check().map_err(|e| e.to_string()));
        match result {
            Ok(()) => grabbed += 1,
            Err(e) => failures.push(format!("mods+{extra:#x}: {e}")),
        }
    }
    if grabbed == 0 {
        return Err(format!(
            "XGrabKey 全部失败(热键可能已被别的程序占用): {}",
            failures.join("; ")
        ));
    }
    if !failures.is_empty() {
        crate::klog!(
            "[热键] 部分锁定键组合注册失败(不影响主流程): {}",
            failures.join("; ")
        );
    }

    // 键位与修饰键完全相同时不能 ungrab——那会把刚抓到的这份也放掉
    if let Some(old) = previous {
        if old.keycode != keycode || old.mods != mods {
            release(conn, root, old);
        }
    }
    crate::klog!(
        "[热键] 已抓取 {accel} (keycode={keycode}, 组合 {grabbed}/{})",
        lock_combos.len()
    );
    Ok(Grab {
        keycode,
        mods,
        lock_combos,
    })
}

/// 运行时改绑全局热键:交给监听线程去做,并同步等它的结果(界面需要知道成没成)。
pub fn set_hotkey(accel: &str) -> XResult<()> {
    *HOTKEY_ACCEL.lock() = accel.to_string();
    let seq = HOTKEY_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let accel = accel.to_string();
    dispatch(move |reply| HotkeyReq::Set { seq, accel, reply })
}

/// 暂停热键(放开已抓的组合):设置界面录快捷键前调用
pub fn pause_hotkey() -> XResult<()> {
    dispatch(|reply| HotkeyReq::Pause { reply })
}

/// 恢复热键(按当前加速键重新抓):录完/取消后调用
pub fn resume_hotkey() -> XResult<()> {
    dispatch(|reply| HotkeyReq::Resume { reply })
}

/// 把请求交给监听线程并同步等结果。
///
/// 请求是**排队**的:并发调用(设置界面连续改绑,或以后有命令改成 async)
/// 不会互相挤掉,每个调用者都拿得到属于自己的那份结果。
fn dispatch(make: impl FnOnce(Sender<XResult<()>>) -> HotkeyReq) -> XResult<()> {
    let (tx, rx) = channel();
    HOTKEY_REQ.lock().push_back(make(tx));
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(result) => result,
        Err(_) => Err("热键监听线程没有响应(可能已退出),本次操作未生效".to_string()),
    }
}

/// 注册全局热键并开始监听,命中时在独立线程里回调 `on_hotkey`。
///
/// 返回 Ok 表示抓键成功(此后由本模块负责触发);
/// 返回 Err 表示抓键不可用,调用方应回退到其它实现。
pub fn start_hotkey_listener<F>(accel: &str, on_hotkey: F) -> XResult<()>
where
    F: Fn() + Send + 'static,
{
    let (conn, root) = connect()?;
    let keymap = KeyMap::load(&conn)?;

    // 被动抓取(owner_events=false)的按键事件按事件掩码投递到根窗口
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new()
            .event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
    )
    .map_err(|e| format!("监听根窗口按键失败: {e}"))?
    .check()
    .map_err(|e| format!("监听根窗口按键失败: {e}"))?;

    *HOTKEY_ACCEL.lock() = accel.to_string();
    let mut current = Some(grab_hotkey(&conn, root, &keymap, accel, None)?);

    std::thread::spawn(move || {
        // 丢弃键盘自动重复。X 服务器在"没开启可检测自动重复"的客户端上,会把重复按键
        // 表示成一对 **同一时间戳** 的 KeyRelease + KeyPress(实测 Xvfb/X.Org 均如此,
        // 且这对事件的时间戳与最初那次按下**并不相同**,不能拿最初的时间戳去比)。
        // 所以这里两条路一起挡:
        //   1. 键已经处于按下状态时,再来的 KeyPress 一定是重复;
        //   2. 紧接着"同键同时间戳的 KeyRelease"出现的 KeyPress 也是重复。
        // 只要抓到这两点中的任意一个,长按热键就只触发一次。
        let mut held: HashMap<u8, u32> = HashMap::new();
        let mut repeat_pair: Option<(u8, u32)> = None;
        loop {
            // 设置界面的请求:改绑 / 暂停 / 恢复。
            // 抓键与放键都必须在本线程做:被动抓键归属发起它的那条 X 连接。
            // 先把请求取出来再处理:锁不能跨越下面的 X11 调用(klog! 也可能很慢)
            let pending = HOTKEY_REQ.lock().pop_front();
            if let Some(req) = pending {
                let (result, reply) = match req {
                    HotkeyReq::Set { seq, accel, reply } => {
                        crate::klog!("[热键] 收到改绑请求 #{seq}: {accel}");
                        let result =
                            match grab_hotkey(&conn, root, &keymap, &accel, current.as_ref()) {
                                Ok(grab) => {
                                    *HOTKEY_ACCEL.lock() = accel;
                                    current = Some(grab);
                                    Ok(())
                                }
                                Err(e) => {
                                    crate::klog!("[热键] 改绑失败: {e}");
                                    Err(e)
                                }
                            };
                        (result, reply)
                    }
                    HotkeyReq::Pause { reply } => {
                        if let Some(grab) = current.take() {
                            release(&conn, root, &grab);
                            crate::klog!(
                                "[热键] 已暂停:放开 keycode={} 的组合,供设置界面录入",
                                grab.keycode
                            );
                        }
                        (Ok(()), reply)
                    }
                    HotkeyReq::Resume { reply } => {
                        let accel = HOTKEY_ACCEL.lock().clone();
                        let result = if accel.is_empty() {
                            Ok(())
                        } else {
                            match grab_hotkey(&conn, root, &keymap, &accel, None) {
                                Ok(grab) => {
                                    current = Some(grab);
                                    Ok(())
                                }
                                Err(e) => {
                                    crate::klog!("[热键] 恢复失败: {e}");
                                    Err(e)
                                }
                            }
                        };
                        (result, reply)
                    }
                };
                let _ = reply.send(result);
            }

            let event = match conn.poll_for_event() {
                Ok(Some(event)) => event,
                // 没有事件就小睡一下(轮询式,才能及时发现上面的改绑请求)
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                Err(e) => {
                    crate::klog!("[热键] X11 事件读取失败,热键监听退出: {e}");
                    // 线程一退出热键就真的失效了:必须把状态改成"不可用",否则设置界面
                    // 还显示"X11 已注册",用户对着失效的快捷键干瞪眼。清掉后端之后,
                    // 用户在设置里重新应用一次热键会重新拉起监听线程(自愈)。
                    crate::hotkey::mark_dead(&format!("X11 事件读取失败: {e}"));
                    break;
                }
            };

            match event {
                Event::KeyPress(event) => {
                    let synthetic = repeat_pair == Some((event.detail, event.time));
                    repeat_pair = None;
                    let was_held = held.insert(event.detail, event.time).is_some();
                    if synthetic || was_held {
                        continue;
                    }
                    if let Some(grab) = current.as_ref() {
                        if event.detail == grab.keycode
                            && u16::from(event.state) & grab.mods == grab.mods
                        {
                            on_hotkey();
                        }
                    }
                }
                Event::KeyRelease(event) => {
                    repeat_pair = Some((event.detail, event.time));
                    held.remove(&event.detail);
                }
                _ => {}
            }
        }
    });

    Ok(())
}

// ---------------------------------------------------------------- 按键注入

/// 用 XTEST 注入 Ctrl+V。事件送给当前拥有输入焦点的窗口。
pub fn send_ctrl_v() -> XResult<()> {
    with_shared(|conn, root| {
        let keymap = KeyMap::load(conn)?;
        let v = keymap
            .keycode_of(b'v' as u32)
            .ok_or_else(|| "键盘映射里找不到按键 V".to_string())?;
        let ctrl = keymap
            .keycode_of(KEYSYM_CONTROL_L)
            .ok_or_else(|| "键盘映射里找不到 Control_L".to_string())?;

        let fake = |keycode: u8, press: bool| -> XResult<()> {
            let type_ = if press {
                KEY_PRESS_EVENT
            } else {
                KEY_RELEASE_EVENT
            };
            xtest::fake_input(conn, type_, keycode, CURRENT_TIME, root, 0, 0, 0)
                .map_err(|e| format!("注入按键失败: {e}"))?
                .check()
                .map_err(|e| format!("注入按键失败: {e}"))
        };

        fake(ctrl, true)?;
        fake(v, true)?;
        std::thread::sleep(Duration::from_millis(12));
        fake(v, false)?;
        fake(ctrl, false)?;
        conn.flush().map_err(|e| format!("刷新 X11 请求失败: {e}"))?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 请求要**排队**、而且各带自己的回传通道。
    ///
    /// 以前请求是单槽(`Option`)、回复也是单槽:`set_hotkey` 与暂停/恢复并发时
    /// 会互相覆盖——被挤掉的那个调用者既收不到结果,又要白等 3 秒,最后误报
    /// "热键监听线程没有响应"。这个用例把那条链路钉住。
    #[test]
    fn queued_requests_keep_their_own_reply_channel() {
        HOTKEY_REQ.lock().clear(); // 清掉进程内可能残留的请求

        let (tx1, rx1) = channel();
        let (tx2, rx2) = channel();
        HOTKEY_REQ.lock().push_back(HotkeyReq::Pause { reply: tx1 });
        HOTKEY_REQ.lock().push_back(HotkeyReq::Resume { reply: tx2 });

        // 模拟监听线程依次取:两个请求都得在,不能互相挤掉,且按入队顺序
        let first = HOTKEY_REQ.lock().pop_front().expect("第一个请求还在");
        let second = HOTKEY_REQ.lock().pop_front().expect("第二个请求也没被挤掉");
        assert!(HOTKEY_REQ.lock().is_empty(), "队列应当已取空");

        let (reply1, reply2) = match (first, second) {
            (HotkeyReq::Pause { reply }, HotkeyReq::Resume { reply: second }) => (reply, second),
            _ => panic!("出队顺序应当与入队顺序一致"),
        };

        // 各自回复:两个调用者拿到的是**自己的**那份结果
        let _ = reply1.send(Err("第一个失败".to_string()));
        let _ = reply2.send(Ok(()));
        assert_eq!(
            rx1.recv_timeout(Duration::from_secs(1)),
            Ok(Err("第一个失败".to_string()))
        );
        assert_eq!(rx2.recv_timeout(Duration::from_secs(1)), Ok(Ok(())));
    }

    /// `dispatch` 必须把**每一个**请求都排进队列。
    ///
    /// 这是老实现的直接病灶:请求是单槽 `Option`,两个并发调用只有后到的能留下,
    /// 前一个既没人处理、也没人回复。这里并发调两次,断言队列里能看到两条
    /// (旧实现下这个断言会失败)。
    #[test]
    fn dispatch_queues_every_request() {
        HOTKEY_REQ.lock().clear();

        // 没有监听线程,所以两个调用最终都会超时返回 Err;这里只关心"有没有都进队列"
        let first = std::thread::spawn(|| dispatch(|reply| HotkeyReq::Pause { reply }));
        let second = std::thread::spawn(|| dispatch(|reply| HotkeyReq::Resume { reply }));

        let deadline = Instant::now() + Duration::from_secs(2);
        while HOTKEY_REQ.lock().len() < 2 {
            assert!(
                Instant::now() < deadline,
                "两个请求没有都进队列(旧实现里后到的会把先到的挤掉)"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // 清空队列会丢掉还没回复的 Sender,两个调用者随即拿到错误、立刻返回,
        // 不用真的等满 3 秒超时
        HOTKEY_REQ.lock().clear();
        assert!(first.join().expect("线程不该 panic").is_err());
        assert!(second.join().expect("线程不该 panic").is_err());
        assert!(HOTKEY_REQ.lock().is_empty());
    }
}
