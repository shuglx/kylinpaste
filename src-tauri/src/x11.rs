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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
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
    let (conn, root) = connect().ok()?;
    let win = active_window(&conn, root)?;
    if window_pid(&conn, win) == Some(std::process::id()) {
        return None;
    }
    window_class(&conn, win)
}

/// 焦点是否已经离开本应用(说明窗口管理器正常地把焦点还给了别的窗口)。
///
/// 返回 false 有两种情况:焦点还在自己身上,或者根本没有活动窗口——
/// 两者都意味着"需要手动归还焦点",否则模拟按键不知道往哪送。
pub fn focus_left_app() -> bool {
    let Ok((conn, root)) = connect() else {
        return false;
    };
    match active_window(&conn, root) {
        Some(win) => window_pid(&conn, win) != Some(std::process::id()),
        None => false,
    }
}

// ---------------------------------------------------------------- 目标窗口

/// 把"当前活动窗口"记为粘贴时的焦点归还目标。
///
/// 如果活动窗口已经是自己(例如剪贴板窗口本来就开着),不覆盖上一次的记录。
/// 建议在**抢占焦点之前**调用(热键/托盘显示窗口前、进程启动时)。
pub fn remember_target_window() {
    let Ok((conn, root)) = connect() else {
        return;
    };
    let Some(win) = active_window(&conn, root) else {
        return;
    };
    if window_pid(&conn, win) == Some(std::process::id()) {
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
    let (conn, root) = match connect() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[X11] {e}");
            return false;
        }
    };
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

    let atom = match intern_atom(&conn, b"_NET_ACTIVE_WINDOW") {
        Ok(atom) => atom,
        Err(e) => {
            eprintln!("[X11] {e}");
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

    if wait_active(&conn, root, win, Duration::from_millis(400)) {
        return true;
    }

    eprintln!("[X11] 窗口管理器未响应 _NET_ACTIVE_WINDOW,回退 XSetInputFocus");
    if let Err(e) = conn
        .set_input_focus(InputFocus::POINTER_ROOT, win, CURRENT_TIME)
        .map_err(|e| e.to_string())
        .and_then(|cookie| cookie.check().map_err(|e| e.to_string()))
    {
        eprintln!("[X11] XSetInputFocus 失败: {e}");
        return false;
    }
    let _ = conn.flush();
    wait_active(&conn, root, win, Duration::from_millis(200))
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

/// 待生效的加速键:监听线程轮询到变化就重新抓键(用来支持运行时改绑)
static HOTKEY_REQ: Lazy<Mutex<Option<(u64, String)>>> = Lazy::new(|| Mutex::new(None));
/// 重新抓键的结果回传通道:`set_hotkey` 同步等结果,好把失败原因报给设置界面
static HOTKEY_REPLY: Lazy<Mutex<Option<Sender<XResult<()>>>>> = Lazy::new(|| Mutex::new(None));
static HOTKEY_SEQ: AtomicU64 = AtomicU64::new(0);

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
        eprintln!(
            "[热键] 部分锁定键组合注册失败(不影响主流程): {}",
            failures.join("; ")
        );
    }

    // 键位与修饰键完全相同时不能 ungrab——那会把刚抓到的这份也放掉
    if let Some(old) = previous {
        if old.keycode != keycode || old.mods != mods {
            for extra in &old.lock_combos {
                let _ = conn.ungrab_key(old.keycode, root, ModMask::from(old.mods | extra));
            }
        }
    }
    println!(
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
    let seq = HOTKEY_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let (tx, rx) = channel();
    *HOTKEY_REPLY.lock() = Some(tx);
    *HOTKEY_REQ.lock() = Some((seq, accel.to_string()));
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(result) => result,
        Err(_) => Err("热键监听线程没有响应,改绑未生效".to_string()),
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
            // 设置界面改了热键 → 重新抓键,并把结果回传给 set_hotkey
            if let Some((seq, accel)) = HOTKEY_REQ.lock().clone() {
                let result = match grab_hotkey(&conn, root, &keymap, &accel, current.as_ref()) {
                    Ok(grab) => {
                        current = Some(grab);
                        Ok(())
                    }
                    Err(e) => Err(e),
                };
                {
                    let mut req = HOTKEY_REQ.lock();
                    if matches!(req.as_ref(), Some((s, _)) if *s == seq) {
                        *req = None;
                    }
                }
                if let Some(tx) = HOTKEY_REPLY.lock().take() {
                    let _ = tx.send(result);
                }
            }

            let event = match conn.poll_for_event() {
                Ok(Some(event)) => event,
                // 没有事件就小睡一下(轮询式,才能及时发现上面的改绑请求)
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                Err(e) => {
                    eprintln!("[热键] X11 事件读取失败,热键监听退出: {e}");
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
    let (conn, root) = connect()?;
    let keymap = KeyMap::load(&conn)?;
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
        xtest::fake_input(&conn, type_, keycode, CURRENT_TIME, root, 0, 0, 0)
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
}
