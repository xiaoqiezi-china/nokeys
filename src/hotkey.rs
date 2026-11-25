use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_LBUTTON, VK_LWIN, VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_RWIN, VK_SHIFT,
    VK_XBUTTON1, VK_XBUTTON2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, KBDLLHOOKSTRUCT, MSLLHOOKSTRUCT, SetWindowsHookExW, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP,
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

/// Update tracked modifier state from a keyboard event.
fn track_modifier(state: &mut HookState, vk: u16, is_down: bool) {
    if vk == VK_CONTROL.0 {
        state.ctrl = is_down;
    } else if vk == VK_MENU.0 {
        state.alt = is_down;
    } else if vk == VK_SHIFT.0 {
        state.shift = is_down;
    } else if vk == VK_LWIN.0 || vk == VK_RWIN.0 {
        state.win = is_down;
    }
}

fn is_modifier(vk: u16) -> bool {
    vk == VK_CONTROL.0
        || vk == VK_SHIFT.0
        || vk == VK_MENU.0
        || vk == VK_LWIN.0
        || vk == VK_RWIN.0
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
            track_modifier(&mut st, info.vkCode as u16, is_down);

            // Rebinding: swallow everything, report the first non-modifier key press.
            if st.capturing {
                if is_down && !is_modifier(info.vkCode as u16) {
                    let hk = st.snapshot(info.vkCode, 0);
                    st.capturing = false;
                    let _ = st.tx.send(HotkeyEvent::Captured(hk));
                }
                return LRESULT(1);
            }

            // Keyboard hotkey handling only applies when the hotkey is a keyboard key.
            if st.hotkey.mouse == 0 {
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

        // Rebinding: swallow mouse buttons and report the pressed one as the hotkey.
        if st.capturing {
            let (down, _up) = mouse_btn_state_any(msg, lparam);
            if let Some(vk) = down {
                let hk = st.snapshot(0, vk);
                st.capturing = false;
                let _ = st.tx.send(HotkeyEvent::Captured(hk));
            }
            return LRESULT(1);
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
    }));

    // Low-level hooks live in the process itself (not a DLL); pass NULL for hMod.
    let kb = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), None, 0) };
    let ms = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(ms_proc), None, 0) };
    eprintln!(
        "[nokeys] hooks installed: kb={} ms={} hotkey={}",
        kb.is_ok(),
        ms.is_ok(),
        hotkey_dbg
    );
}

/// Toggle rebinding capture mode. While active every key/mouse event is swallowed
/// and the first non-modifier key press is reported as [`HotkeyEvent::Captured`].
pub fn set_capturing(on: bool) {
    if let Some(state) = HOOK_STATE.get() {
        if let Ok(mut st) = state.lock() {
            st.capturing = on;
        }
    }
}

/// Replace the active hotkey (e.g. after a rebind).
pub fn set_hotkey(hotkey: Hotkey) {
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
