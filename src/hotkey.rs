use std::sync::mpsc::Sender;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_LBUTTON, VK_LWIN, VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_RWIN, VK_SHIFT,
    VK_XBUTTON1, VK_XBUTTON2,
};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, WPARAM as WP};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, RegisterClassW,
    KBDLLHOOKSTRUCT, MSLLHOOKSTRUCT, SetWindowsHookExW, UnhookWindowsHookEx,
    CW_USEDEFAULT, HHOOK, HWND_MESSAGE,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE, WNDCLASSW, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL,
    MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};

use crate::config::Hotkey;

/// Events emitted by the global hotkey hook.
#[derive(Debug, Clone, Copy)]
pub enum HotkeyEvent {
    /// Hotkey chord became fully pressed (open overlay).
    Down,
    /// Hotkey released.
    Up,
    /// Mouse wheel delta while the overlay is engaged (for wheel switching).
    Wheel(i32),
    /// A candidate hotkey captured while the settings dialog is rebinding.
    Captured(Hotkey),
    /// Rebinding aborted (Esc).
    CaptureCancelled,
}

struct HookState {
    engaged: bool,
    hotkey: Hotkey,
    tx: Sender<HotkeyEvent>,
    // modifier states tracked from the hook stream itself
    ctrl: bool,
    alt: bool,
    shift: bool,
    win: bool,
    // rebinding mode: report the next chord and swallow everything
    capturing: bool,
    capture_started: std::time::Instant,
    debug: bool,
}

impl HookState {
    fn mods_match(&self, h: &Hotkey) -> bool {
        self.ctrl == h.ctrl && self.alt == h.alt && self.shift == h.shift && self.win == h.win
    }

    fn snapshot(&self, key: u32, mouse: u32) -> Hotkey {
        Hotkey {
            ctrl: self.ctrl,
            alt: self.alt,
            shift: self.shift,
            win: self.win,
            key,
            mouse,
        }
    }
}

static HOOK_STATE: OnceLock<Mutex<HookState>> = OnceLock::new();

// Lock-free counters so the UI thread can verify hook liveness without the hook
// ever performing I/O.
static KB_HOOK: AtomicIsize = AtomicIsize::new(0);
static MS_HOOK: AtomicIsize = AtomicIsize::new(0);

// The low-level keyboard hook proved unreliable on some Windows setups (the OS
// silently drops it), so the primary trigger is a system-wide RegisterHotKey on a
// hidden message-only window, with a polling thread detecting the key release.
static TRIGGER_HWND: AtomicIsize = AtomicIsize::new(0);
static TRIGGER_ACTIVE: AtomicU32 = AtomicU32::new(0);
static TRIGGER_HELD: AtomicU32 = AtomicU32::new(0);
static TRIGGER_VK: AtomicU32 = AtomicU32::new(0);
static EVENT_TX: OnceLock<Sender<HotkeyEvent>> = OnceLock::new();

static STAT_KEYS: AtomicU32 = AtomicU32::new(0);
static STAT_CAPTURE_ACTIVE: AtomicU32 = AtomicU32::new(0);
static STAT_CAPTURE_HITS: AtomicU32 = AtomicU32::new(0);
static STAT_CAPTURE_ON: AtomicU32 = AtomicU32::new(0);

pub fn stat_keys() -> u32 {
    STAT_KEYS.load(Ordering::Relaxed)
}
pub fn stat_capture_active() -> u32 {
    STAT_CAPTURE_ACTIVE.load(Ordering::Relaxed)
}
pub fn stat_capture_hits() -> u32 {
    STAT_CAPTURE_HITS.load(Ordering::Relaxed)
}

unsafe extern "system" fn sink_proc(hwnd: HWND, msg: u32, wparam: WP, lparam: LPARAM) -> LRESULT {
    if msg == WM_HOTKEY {
        // RegisterHotKey already swallowed the key; report the press and let the
        // poller notice the release.
        if TRIGGER_HELD.swap(1, Ordering::SeqCst) == 0 {
            if let Some(tx) = EVENT_TX.get() {
                let _ = tx.send(HotkeyEvent::Down);
            }
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Create (once) the hidden window that receives WM_HOTKEY. Must run on the thread
/// that pumps messages.
fn ensure_sink() -> Option<HWND> {
    let existing = TRIGGER_HWND.load(Ordering::SeqCst);
    if existing != 0 {
        return Some(HWND(existing as *mut _));
    }
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let class = w!("NoKeysHotkeySink");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(sink_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        // A duplicate class registration just fails and is harmless.
        let _ = RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR::null(),
            Default::default(),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            Some(HWND_MESSAGE),
            None,
            Some(hinst.into()),
            None,
        )
        .ok()?;
        TRIGGER_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        Some(hwnd)
    }
}

/// (Re)register the global hotkey. Falls back to the keyboard hook when the chord
/// is already owned by another application.
pub fn register_trigger(hotkey: &Hotkey) {
    TRIGGER_ACTIVE.store(0, Ordering::SeqCst);
    unsafe {
        let old = TRIGGER_HWND.load(Ordering::SeqCst);
        if old != 0 {
            let _ = UnregisterHotKey(Some(HWND(old as *mut _)), 1);
        }
    }
    if hotkey.mouse != 0 {
        crate::ntrace!("trigger: mouse hotkey, low-level hook only");
        return;
    }
    let Some(hwnd) = ensure_sink() else {
        crate::ntrace!("trigger: no sink window, falling back to hook");
        return;
    };
    let mods = HOT_KEY_MODIFIERS(
        0 | if hotkey.alt { MOD_ALT.0 } else { 0 }
            | if hotkey.ctrl { MOD_CONTROL.0 } else { 0 }
            | if hotkey.shift { MOD_SHIFT.0 } else { 0 }
            | if hotkey.win { MOD_WIN.0 } else { 0 }
            | MOD_NOREPEAT.0,
    );
    let ok = unsafe { RegisterHotKey(Some(hwnd), 1, mods, hotkey.key) };
    if ok.is_ok() {
        TRIGGER_ACTIVE.store(1, Ordering::SeqCst);
        TRIGGER_VK.store(hotkey.key as u32, Ordering::SeqCst);
            crate::ntrace!("trigger: RegisterHotKey OK (vk={})", hotkey.key);
    } else {
        crate::ntrace!(
            "trigger: RegisterHotKey FAILED ({:?}), falling back to low-level hook",
            ok
        );
    }
}

/// Single background poller. It does two hook-free jobs:
///   1. report `Up` once the trigger key is released (RegisterHotKey has no key-up),
///   2. capture a new hotkey by watching `GetAsyncKeyState` edges, which keeps
///      rebinding working even when the OS drops our low-level keyboard hook.
pub fn start_input_poller() {
    std::thread::spawn(|| {
        const VK_CONTROL_G: i32 = 0x11;
        const VK_SHIFT_G: i32 = 0x10;
        const VK_MENU_G: i32 = 0x12;
        const VK_LWIN_G: i32 = 0x5B;
        const VK_RWIN_G: i32 = 0x5C;

        let down = |vk: i32| -> bool { unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 } };

        let mut prev = [false; 256];
        let mut was_capturing = false;

        loop {
            std::thread::sleep(std::time::Duration::from_millis(8));

            // 1. trigger key release
            if TRIGGER_ACTIVE.load(Ordering::SeqCst) == 1 && TRIGGER_HELD.load(Ordering::SeqCst) == 1
            {
                let vk = TRIGGER_VK.load(Ordering::SeqCst) as i32;
                if !down(vk) && TRIGGER_HELD.swap(0, Ordering::SeqCst) == 1 {
                    if let Some(tx) = EVENT_TX.get() {
                        let _ = tx.send(HotkeyEvent::Up);
                    }
                }
            }

            // 2. rebinding capture
            let capturing = STAT_CAPTURE_ON.load(Ordering::SeqCst) == 1;
            if !capturing {
                if was_capturing {
                    // Reset the edge detector for the next capture session.
                    prev = [false; 256];
                    was_capturing = false;
                }
                continue;
            }
            if !was_capturing {
                // Seed with keys already held so a still-pressed chord cannot
                // immediately become the new hotkey.
                for vk in 0..256usize {
                    prev[vk] = down(vk as i32);
                }
                was_capturing = true;
                continue;
            }

            let ctrl = down(VK_CONTROL_G);
            let alt = down(VK_MENU_G);
            let shift = down(VK_SHIFT_G);
            let win = down(VK_LWIN_G) || down(VK_RWIN_G);

            for vk in 1u32..=254 {
                let is_down = down(vk as i32);
                let was = prev[vk as usize];
                prev[vk as usize] = is_down;
                if !is_down || was {
                    continue;
                }
                // Fresh press edge.
                let is_mouse = (1..=6).contains(&vk);
                let modifier = matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5);
                if modifier {
                    continue;
                }
                if is_mouse && (vk == 1 || vk == 2) {
                    continue; // left/right must stay usable for the UI
                }
                if let Some(tx) = EVENT_TX.get() {
                    if vk == 0x1B {
                        let _ = tx.send(HotkeyEvent::CaptureCancelled);
                        STAT_CAPTURE_ON.store(0, Ordering::SeqCst);
                        continue;
                    }
                    let hk = if is_mouse {
                        Hotkey { ctrl, alt, shift, win, key: 0, mouse: vk }
                    } else {
                        Hotkey { ctrl, alt, shift, win, key: vk, mouse: 0 }
                    };
                    if !chord_acceptable(&hk) {
                        // Keep waiting: a bare letter would break typing system-wide.
                        crate::ntrace!("capture rejected bare vk={vk} (needs a modifier)");
                        continue;
                    }
                    let _ = tx.send(HotkeyEvent::Captured(hk));
                    STAT_CAPTURE_ON.store(0, Ordering::SeqCst);
                }
                break;
            }
        }
    });
}

// Low-level hooks report the *side-specific* virtual key codes (VK_LCONTROL etc.),
// so both the generic and the left/right variants must be folded into one flag.
const VK_LSHIFT_C: u16 = 0xA0;
const VK_RSHIFT_C: u16 = 0xA1;
const VK_LCONTROL_C: u16 = 0xA2;
const VK_RCONTROL_C: u16 = 0xA3;
const VK_LMENU_C: u16 = 0xA4;
const VK_RMENU_C: u16 = 0xA5;

/// Update tracked modifier state from a keyboard event.
fn track_modifier(state: &mut HookState, vk: u16, is_down: bool) {
    match vk {
        VK_LSHIFT_C | VK_RSHIFT_C => state.shift = is_down,
        VK_LCONTROL_C | VK_RCONTROL_C => state.ctrl = is_down,
        VK_LMENU_C | VK_RMENU_C => state.alt = is_down,
        v if v == VK_SHIFT.0 => state.shift = is_down,
        v if v == VK_CONTROL.0 => state.ctrl = is_down,
        v if v == VK_MENU.0 => state.alt = is_down,
        v if v == VK_LWIN.0 || v == VK_RWIN.0 => state.win = is_down,
        _ => {}
    }
}

fn is_modifier(vk: u16) -> bool {
    matches!(vk, VK_LSHIFT_C | VK_RSHIFT_C | VK_LCONTROL_C | VK_RCONTROL_C | VK_LMENU_C | VK_RMENU_C)
        || vk == VK_SHIFT.0
        || vk == VK_CONTROL.0
        || vk == VK_MENU.0
        || vk == VK_LWIN.0
        || vk == VK_RWIN.0
}

/// A bare letter/digit/punctuation key can never be a sane global hotkey: the
/// system hotkey swallows it everywhere, which would break typing app-wide.
/// Function keys and above (F1-F24, media, browser keys) are fine on their own.
fn bare_key_allowed(vk: u32) -> bool {
    vk >= 0x70 || vk == 0x2C /* PrintScreen */
}

fn chord_acceptable(hk: &Hotkey) -> bool {
    if hk.mouse != 0 {
        return true;
    }
    hk.ctrl || hk.alt || hk.win || bare_key_allowed(hk.key)
}

/// Detect any mouse button press, for hotkey rebinding.
fn mouse_btn_state_any(msg: u32, lparam: LPARAM) -> (Option<u32>, bool) {
    match msg {
        WM_LBUTTONDOWN => (Some(VK_LBUTTON.0 as u32), false),
        WM_RBUTTONDOWN => (Some(VK_RBUTTON.0 as u32), false),
        WM_MBUTTONDOWN => (Some(VK_MBUTTON.0 as u32), false),
        WM_XBUTTONDOWN => {
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let which = (info.mouseData >> 16) & 0xFFFF;
            let vk = if which == 1 {
                VK_XBUTTON1.0 as u32
            } else {
                VK_XBUTTON2.0 as u32
            };
            (Some(vk), false)
        }
        _ => (None, false),
    }
}

/// Given a mouse-button virtual key and a mouse hook message, return (is_down, is_up).
fn mouse_btn_state(mouse_vk: u32, msg: u32, lparam: LPARAM) -> (bool, bool) {
    match mouse_vk as u16 {
        x if x == VK_LBUTTON.0 => (msg == WM_LBUTTONDOWN, msg == WM_LBUTTONUP),
        x if x == VK_RBUTTON.0 => (msg == WM_RBUTTONDOWN, msg == WM_RBUTTONUP),
        x if x == VK_MBUTTON.0 => (msg == WM_MBUTTONDOWN, msg == WM_MBUTTONUP),
        x if x == VK_XBUTTON1.0 || x == VK_XBUTTON2.0 => {
            if msg != WM_XBUTTONDOWN && msg != WM_XBUTTONUP {
                return (false, false);
            }
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let which = (info.mouseData >> 16) & 0xFFFF;
            if which as u16 != x {
                return (false, false);
            }
            (msg == WM_XBUTTONDOWN, msg == WM_XBUTTONUP)
        }
        _ => (false, false),
    }
}

unsafe extern "system" fn kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wparam.0 as u32;
        let guard = HOOK_STATE.get().expect("hook state");
        let mut st = guard.lock().unwrap();

        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

        if is_down || is_up {
            let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if is_down {
                STAT_KEYS.fetch_add(1, Ordering::Relaxed);
            }
            track_modifier(&mut st, info.vkCode as u16, is_down);
            if st.debug {
                eprintln!(
                    "[hook] vk={:#04x} down={} mods(ctrl={} alt={} shift={} win={}) engaged={} capturing={}",
                    info.vkCode, is_down, st.ctrl, st.alt, st.shift, st.win, st.engaged, st.capturing
                );
            }

            // Rebinding: swallow everything, report the first non-modifier key press.
            if st.capturing {
                STAT_CAPTURE_ACTIVE.fetch_add(1, Ordering::Relaxed);
                let settled = st.capture_started.elapsed().as_millis() > 350;
                if is_down && settled && info.vkCode == 0x1B {
                    // Esc aborts rebinding
                    st.capturing = false;
                    let _ = st.tx.send(HotkeyEvent::CaptureCancelled);
                } else if is_down && settled && !is_modifier(info.vkCode as u16) {
                    let hk = st.snapshot(info.vkCode, 0);
                    if !chord_acceptable(&hk) {
                        return LRESULT(1);
                    }
                    STAT_CAPTURE_HITS.fetch_add(1, Ordering::Relaxed);
                    st.capturing = false;
                    let _ = st.tx.send(HotkeyEvent::Captured(hk));
                }
                return LRESULT(1);
            }

            // Keyboard hotkey handling only applies when the hotkey is a keyboard key.
            if st.hotkey.mouse == 0 && TRIGGER_ACTIVE.load(Ordering::Relaxed) == 0 {
                if st.engaged {
                    // While engaged, swallow the whole keyboard stream so the chord
                    // (including modifier releases) never reaches the foreground app.
                    if is_up && info.vkCode == st.hotkey.key {
                        st.engaged = false;
                        let _ = st.tx.send(HotkeyEvent::Up);
                    }
                    return LRESULT(1);
                } else if is_down
                    && info.vkCode == st.hotkey.key
                    && st.mods_match(&st.hotkey)
                {
                    st.engaged = true;
                    let _ = st.tx.send(HotkeyEvent::Down);
                    return LRESULT(1);
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn ms_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wparam.0 as u32;
        let guard = HOOK_STATE.get().expect("hook state");
        let mut st = guard.lock().unwrap();

        // Wheel switching while the overlay is engaged.
        if st.engaged && msg == WM_MOUSEWHEEL {
            let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            let delta = ((info.mouseData >> 16) & 0xFFFF) as i16 as i32;
            let _ = st.tx.send(HotkeyEvent::Wheel(delta));
            return LRESULT(1);
        }

        // Rebinding: only consume a middle/side button press as the new hotkey.
        // Everything else (cursor moves, left/right clicks, wheel) must pass through,
        // otherwise the cursor freezes and the dialog becomes unusable.
        if st.capturing {
            let settled = st.capture_started.elapsed().as_millis() > 350;
            if settled {
                if let (Some(vk), _) = mouse_btn_state_any(msg, lparam) {
                    if vk != VK_LBUTTON.0 as u32 && vk != VK_RBUTTON.0 as u32 {
                        let hk = st.snapshot(0, vk);
                        st.capturing = false;
                        let _ = st.tx.send(HotkeyEvent::Captured(hk));
                        return LRESULT(1);
                    }
                }
            }
        }

        // Mouse-button hotkey handling.
        if st.hotkey.mouse != 0 {
            let (down, up) = mouse_btn_state(st.hotkey.mouse, msg, lparam);
            if st.engaged {
                if up {
                    st.engaged = false;
                    let _ = st.tx.send(HotkeyEvent::Up);
                    return LRESULT(1);
                }
                if down {
                    return LRESULT(1); // swallow repeated down while held
                }
            } else if down && st.mods_match(&st.hotkey) {
                st.engaged = true;
                let _ = st.tx.send(HotkeyEvent::Down);
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Install both low-level hooks on the calling thread. The calling thread must run
/// a Windows message pump afterwards (the Slint event loop does).
pub fn install(tx: Sender<HotkeyEvent>, hotkey: Hotkey) {
    let _ = EVENT_TX.set(tx.clone());
    let hotkey_dbg = format!("{hotkey:?}");
    let _ = HOOK_STATE.set(Mutex::new(HookState {
        engaged: false,
        hotkey,
        tx,
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
        capturing: false,
        capture_started: std::time::Instant::now(),
        debug: std::env::var("NOKEYS_DEBUG_HOOK").is_ok(),
    }));

    install_hooks();
    register_trigger(&hotkey);
    start_input_poller();
    eprintln!("[nokeys] hooks installed: hotkey={hotkey_dbg}");
}

fn install_hooks() {
    // Low-level hooks live in the process itself (not a DLL); pass NULL for hMod.
    // They must be installed from the thread that pumps messages (the UI thread).
    let kb = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), None, 0) };
    let ms = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(ms_proc), None, 0) };
    KB_HOOK.store(kb.map(|h| h.0 as isize).unwrap_or(0), Ordering::Relaxed);
    MS_HOOK.store(ms.map(|h| h.0 as isize).unwrap_or(0), Ordering::Relaxed);
}

/// Re-install both hooks. Windows silently removes a low-level hook whose callback
/// exceeded the timeout, and there is no way to detect that, so we refresh
/// periodically. Must run on the event-loop (message-pumping) thread.
pub fn reinstall_hooks() {
    unsafe {
        let kb = KB_HOOK.swap(0, Ordering::Relaxed);
        let ms = MS_HOOK.swap(0, Ordering::Relaxed);
        if kb != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(kb as *mut _));
        }
        if ms != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(ms as *mut _));
        }
    }
    install_hooks();
    crate::ntrace!(
        "hooks reinstalled kb={} ms={}",
        KB_HOOK.load(Ordering::Relaxed) != 0,
        MS_HOOK.load(Ordering::Relaxed) != 0
    );
}

/// Toggle rebinding capture mode. While active every key/mouse event is swallowed
/// and the first non-modifier key press is reported as [`HotkeyEvent::Captured`].
pub fn set_capturing(on: bool) {
    crate::ntrace!("hotkey::set_capturing({on}) state={}", HOOK_STATE.get().is_some());
    STAT_CAPTURE_ON.store(if on { 1 } else { 0 }, Ordering::SeqCst);
    if let Some(state) = HOOK_STATE.get() {
        if let Ok(mut st) = state.lock() {
            st.capturing = on;
            st.capture_started = std::time::Instant::now();
            crate::ntrace!("  hook capturing={}", st.capturing);
        }
    }
}

/// Replace the active hotkey (e.g. after a rebind).
pub fn set_hotkey(hotkey: Hotkey) {
    TRIGGER_HELD.store(0, Ordering::SeqCst);
    register_trigger(&hotkey);
    if let Some(state) = HOOK_STATE.get() {
        if let Ok(mut st) = state.lock() {
            st.hotkey = hotkey;
            st.engaged = false;
            st.ctrl = false;
            st.alt = false;
            st.shift = false;
            st.win = false;
        }
    }
}

/// Offline self-test: feed synthetic keyboard events through the real hook
/// callbacks to validate hotkey matching without needing OS-level input.
/// Returns a list of (step, matched) pairs.
#[cfg(debug_assertions)]
pub fn selftest(hotkey: Hotkey) -> Vec<(String, bool)> {
    use std::sync::mpsc::channel;
    use windows::Win32::UI::WindowsAndMessaging::KBDLLHOOKSTRUCT;

    let (tx, rx) = channel();
    let _ = HOOK_STATE.set(Mutex::new(HookState {
        engaged: false,
        hotkey,
        tx,
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
        capturing: false,
        capture_started: std::time::Instant::now(),
        debug: false,
    }));

    // vk codes as the OS would report them (side-specific modifiers)
    let (ctrl_vk, alt_vk) = (0xA2u32, 0xA4u32);
    let mut out = Vec::new();

    let mut send = |vk: u32, down: bool| -> Vec<HotkeyEvent> {
        let info = KBDLLHOOKSTRUCT {
            vkCode: vk,
            ..Default::default()
        };
        let msg = if down { WM_KEYDOWN } else { WM_KEYUP };
        unsafe {
            kb_proc(0, WPARAM(msg as usize), LPARAM(&info as *const _ as isize));
        }
        rx.try_iter().collect()
    };

    if hotkey.ctrl {
        send(ctrl_vk, true);
    }
    if hotkey.alt {
        send(alt_vk, true);
    }
    let evs = send(hotkey.key, true);
    out.push((
        "chord down emits Down".into(),
        evs.iter().any(|e| matches!(e, HotkeyEvent::Down)),
    ));

    // a second keydown while engaged must not emit another Down
    let evs = send(hotkey.key, true);
    out.push((
        "repeat while held emits nothing".into(),
        !evs.iter().any(|e| matches!(e, HotkeyEvent::Down)),
    ));

    let evs = send(hotkey.key, false);
    out.push((
        "key up emits Up".into(),
        evs.iter().any(|e| matches!(e, HotkeyEvent::Up)),
    ));

    if hotkey.alt {
        send(alt_vk, false);
    }
    if hotkey.ctrl {
        send(ctrl_vk, false);
    }

    // pressing the key alone (no modifiers) must not trigger
    let evs = send(hotkey.key, true);
    out.push((
        "bare key does not trigger".into(),
        !evs.iter().any(|e| matches!(e, HotkeyEvent::Down)),
    ));
    send(hotkey.key, false);

    out
}
