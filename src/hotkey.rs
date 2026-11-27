// Global hotkey handling.
//
// There are exactly three moving parts, each with one job:
//
//  * A polling thread reading GetAsyncKeyState is the ONLY source of the
//    trigger. It reports one Down when the chord becomes fully pressed and one
//    Up when any part of it is released. Because a single thread owns the
//    edges, the rest of the program can never observe a duplicated or dropped
//    press, no matter what the hooks do.
//  * The low-level hooks never emit trigger events. They only swallow the chord
//    so the foreground application never sees it, swallow the rest of the
//    keyboard while the overlay is engaged, and forward wheel deltas (which
//    polling cannot observe).
// Exactly one of those two paths may own the trigger at a time, because a hook
// that swallows the chord also keeps it out of the async key state: the poller
// watches a heartbeat from the hooks and stands down while they are alive.
// RegisterHotKey is deliberately NOT used: it can consume the chord without the
// application ever seeing a matching key-up, which leaves the overlay stuck open.
//
// Rebinding is handled by the same poller: it watches for key edges and turns
// the first acceptable one into Captured.

use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
    MSLLHOOKSTRUCT, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_KEYUP,
    WM_MOUSEWHEEL, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::config::Hotkey;

/// Events produced by the hotkey layer. All of them originate from the polling
/// thread, except Wheel, which comes from the mouse hook.
#[derive(Debug, Clone, Copy)]
pub enum HotkeyEvent {
    /// The chord became fully pressed.
    Down,
    /// Any part of the chord was released.
    Up,
    /// Mouse wheel delta while the overlay is engaged (wheel switching).
    Wheel(i32),
    /// A new hotkey was captured during rebinding.
    Captured(Hotkey),
    /// Rebinding was aborted with Esc (or timed out).
    CaptureCancelled,
    /// A key that cannot be used as a hotkey was pressed while rebinding.
    /// 1 = needs a modifier, 2 = that button is reserved for normal use.
    CaptureRejected(u8),
}

// ----- chord, kept in atomics so both the poller and the hooks can read it
// without ever taking a lock (a hook callback must not block) -----
const M_CTRL: u32 = 1;
const M_ALT: u32 = 2;
const M_SHIFT: u32 = 4;
const M_WIN: u32 = 8;

static MODS: AtomicU32 = AtomicU32::new(0);
static KEY_VK: AtomicU32 = AtomicU32::new(0);
static MOUSE_VK: AtomicU32 = AtomicU32::new(0);
/// 1 while the chord is held down; guards the Down/Up edges.
static HELD: AtomicU32 = AtomicU32::new(0);
/// 1 while presses must be ignored until the chord is seen released again.
/// Set after a rebind, where the keys that produced the chord are still down.
/// It can only ever delay one press: every observed chord-up clears it.
static SUPPRESS: AtomicU32 = AtomicU32::new(0);
/// 1 while the overlay is on screen because of the hotkey, which is the only
/// situation in which the rest of the keyboard is taken away from the user.
/// Driven by the UI and never by internal state, so it cannot get stuck: the
/// worst case is that the overlay is still visible while it is set.
static SWALLOW: AtomicU32 = AtomicU32::new(0);

/// 1 while the settings dialog is rebinding.
static CAPTURING: AtomicU32 = AtomicU32::new(0);
/// Bumped on every capture start so the poller can re-seed its edge detector.
static CAPTURE_SEQ: AtomicU64 = AtomicU64::new(0);

static EVENT_TX: OnceLock<Sender<HotkeyEvent>> = OnceLock::new();
static KB_HOOK: AtomicIsize = AtomicIsize::new(0);
static MS_HOOK: AtomicIsize = AtomicIsize::new(0);
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);

// Liveness counters for the diagnostics thread; never do file I/O from a hook.
static STAT_KEYS: AtomicU32 = AtomicU32::new(0);
static STAT_MOUSE: AtomicU32 = AtomicU32::new(0);
static STAT_SWALLOWED: AtomicU32 = AtomicU32::new(0);
// Heartbeats bumped by the hook callbacks. The poller watches them to decide
// whether the hooks are actually being called on this machine: a hook that
// swallows the chord also hides it from GetAsyncKeyState, so exactly one of the
// two paths may be allowed to own the trigger at a time.
static KB_BEAT: AtomicU32 = AtomicU32::new(0);
static MS_BEAT: AtomicU32 = AtomicU32::new(0);
static STAT_CAPTURE_HITS: AtomicU32 = AtomicU32::new(0);

pub fn stat_keys() -> u32 {
    STAT_KEYS.load(Ordering::Relaxed)
}
pub fn stat_capture_active() -> u32 {
    CAPTURING.load(Ordering::Relaxed)
}
pub fn stat_capture_hits() -> u32 {
    STAT_CAPTURE_HITS.load(Ordering::Relaxed)
}
pub fn stat_hook_rounds() -> u32 {
    KB_BEAT.load(Ordering::Relaxed)
}
pub fn stat_mouse() -> u32 {
    STAT_MOUSE.load(Ordering::Relaxed)
}
pub fn stat_swallowed() -> u32 {
    STAT_SWALLOWED.load(Ordering::Relaxed)
}

// ----- helpers -----

#[inline]
fn key_down(vk: u32) -> bool {
    if vk == 0 || vk > 255 {
        return false;
    }
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

fn mods_now() -> u32 {
    let mut bits = 0;
    if key_down(VK_CONTROL.0 as u32) {
        bits |= M_CTRL;
    }
    if key_down(VK_MENU.0 as u32) {
        bits |= M_ALT;
    }
    if key_down(VK_SHIFT.0 as u32) {
        bits |= M_SHIFT;
    }
    if key_down(VK_LWIN.0 as u32) || key_down(VK_RWIN.0 as u32) {
        bits |= M_WIN;
    }
    bits
}

/// True when every modifier the chord requires is currently held.
fn mods_satisfied(bits: u32) -> bool {
    (bits & M_CTRL == 0 || key_down(VK_CONTROL.0 as u32))
        && (bits & M_ALT == 0 || key_down(VK_MENU.0 as u32))
        && (bits & M_SHIFT == 0 || key_down(VK_SHIFT.0 as u32))
        && (bits & M_WIN == 0 || key_down(VK_LWIN.0 as u32) || key_down(VK_RWIN.0 as u32))
}

fn chord_down() -> bool {
    let mouse = MOUSE_VK.load(Ordering::Relaxed);
    let key = KEY_VK.load(Ordering::Relaxed);
    let target = if mouse != 0 { mouse } else { key };
    if target == 0 {
        return false;
    }
    key_down(target) && mods_satisfied(MODS.load(Ordering::Relaxed))
}

fn emit(ev: HotkeyEvent) {
    if let Some(tx) = EVENT_TX.get() {
        let _ = tx.send(ev);
    }
}

/// Report the chord as pressed. Idempotent: repeated calls while held are no-ops.
fn press() {
    if HELD.swap(1, Ordering::SeqCst) == 0 {
        crate::ntrace!("press -> Down");
        emit(HotkeyEvent::Down);
    }
}

/// Report the chord as released. Idempotent, and always clears the rebind
/// suppression so the next real press works again.
fn release() {
    SUPPRESS.store(0, Ordering::SeqCst);
    if HELD.swap(0, Ordering::SeqCst) == 1 {
        crate::ntrace!("release -> Up");
        emit(HotkeyEvent::Up);
    }
}

fn is_modifier(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

/// True for the mouse buttons we accept as a hotkey. Left and right are
/// excluded: they must stay usable for ordinary clicking, including on our own
/// settings dialog.
fn is_mouse_button(vk: u32) -> bool {
    matches!(vk, 0x04 | 0x05 | 0x06)
}

/// Keys that produce no text and can therefore be a hotkey on their own.
/// A bare letter, digit or punctuation key would have to be swallowed
/// system-wide, which would break typing in every application, so those always
/// need Ctrl/Alt/Win. Mouse buttons are handled separately and never need a
/// modifier either.
fn bare_key_allowed(vk: u32) -> bool {
    matches!(vk,
        // F1-F24
        0x70..=0x87
        // PrintScreen, ScrollLock, Pause
        | 0x2C | 0x91 | 0x13
        // browser, volume, media and app-launch keys
        | 0xA6..=0xB7
    )
}

fn chord_acceptable(hk: &Hotkey) -> bool {
    if hk.mouse != 0 {
        return true;
    }
    hk.ctrl || hk.alt || hk.win || bare_key_allowed(hk.key)
}

/// Turn a fresh key press into a chord, or None when it must be ignored
/// (modifier, left/right mouse button, or a bare key that types).
fn classify(vk: u32, mods: u32) -> Option<Hotkey> {
    if is_modifier(vk) || vk == 0x01 || vk == 0x02 {
        return None;
    }
    let hk = Hotkey {
        ctrl: mods & M_CTRL != 0,
        alt: mods & M_ALT != 0,
        shift: mods & M_SHIFT != 0,
        win: mods & M_WIN != 0,
        key: if is_mouse_button(vk) { 0 } else { vk },
        mouse: if is_mouse_button(vk) { vk } else { 0 },
    };
    if chord_acceptable(&hk) {
        Some(hk)
    } else {
        None
    }
}

/// Why a fresh key press cannot become a hotkey, or None when it simply is not
/// a candidate yet (a modifier on its way to completing a chord).
fn reject_reason(vk: u32, mods: u32) -> Option<u8> {
    if is_modifier(vk) {
        return None;
    }
    if vk == 0x01 || vk == 0x02 {
        return Some(2);
    }
    if classify(vk, mods).is_none() {
        return Some(1);
    }
    None
}

// ----- low-level hooks: they swallow, they never report -----

unsafe extern "system" fn kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wparam.0 as u32;
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if is_down || is_up {
            KB_BEAT.fetch_add(1, Ordering::Relaxed);
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let vk = info.vkCode as u32;
            if is_down {
                STAT_KEYS.fetch_add(1, Ordering::Relaxed);
            }

            // Escape always gives the keyboard back. It is the one key that must
            // work even when everything else is being swallowed.
            if SWALLOW.load(Ordering::Relaxed) == 1 && vk == 0x1B {
                release();
                return LRESULT(1);
            }
            // The hook owns the trigger whenever it is running: it also swallows
            // the chord, which keeps the key out of the async key state, so the
            // poller could never observe the press. Never while rebinding, or the
            // capture dialog would open the overlay it is trying to configure.
            if CAPTURING.load(Ordering::Relaxed) == 0
                && MOUSE_VK.load(Ordering::Relaxed) == 0
                && vk == KEY_VK.load(Ordering::Relaxed)
            {
                if is_up && HELD.load(Ordering::Relaxed) == 1 {
                    STAT_SWALLOWED.fetch_add(1, Ordering::Relaxed);
                    release();
                    return LRESULT(1);
                }
                if is_down && HELD.load(Ordering::Relaxed) == 0 && mods_satisfied(MODS.load(Ordering::Relaxed))
                {
                    press();
                    return LRESULT(1);
                }
            }
            // While the overlay is up the whole keyboard stays with us. This is
            // tied to the overlay being visible, not to any internal flag: the
            // old sticky state is what used to leave machines with no keyboard.
            if SWALLOW.load(Ordering::Relaxed) == 1 && HELD.load(Ordering::Relaxed) == 1 {
                STAT_SWALLOWED.fetch_add(1, Ordering::Relaxed);
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn ms_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    STAT_MOUSE.fetch_add(1, Ordering::Relaxed);
    MS_BEAT.fetch_add(1, Ordering::Relaxed);
    if code >= 0 {
        let msg = wparam.0 as u32;
        if msg == WM_MOUSEWHEEL && HELD.load(Ordering::Relaxed) == 1 && SWALLOW.load(Ordering::Relaxed) == 1 {
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let delta = ((info.mouseData >> 16) & 0xFFFF) as i16 as i32;
            emit(HotkeyEvent::Wheel(delta));
            return LRESULT(1);
        }
        let chord_mouse = MOUSE_VK.load(Ordering::Relaxed);
        if chord_mouse != 0 && CAPTURING.load(Ordering::Relaxed) == 0 {
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let (button, down, up) = mouse_msg(msg, info);
            if button == chord_mouse && (down || up) {
                if up && HELD.load(Ordering::Relaxed) == 1 {
                    release();
                    return LRESULT(1);
                }
                if down && HELD.load(Ordering::Relaxed) == 0 && mods_satisfied(MODS.load(Ordering::Relaxed))
                {
                    press();
                    return LRESULT(1);
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Decode a low-level mouse message into (virtual key, is_down, is_up).
fn mouse_msg(msg: u32, info: &MSLLHOOKSTRUCT) -> (u32, bool, bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
        WM_XBUTTONDOWN, WM_XBUTTONUP,
    };
    match msg {
        WM_LBUTTONDOWN => (0x01, true, false),
        WM_LBUTTONUP => (0x01, false, true),
        WM_RBUTTONDOWN => (0x02, true, false),
        WM_RBUTTONUP => (0x02, false, true),
        WM_MBUTTONDOWN => (0x04, true, false),
        WM_MBUTTONUP => (0x04, false, true),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let which = ((info.mouseData >> 16) & 0xFFFF) as u32;
            let vk = if which == 1 { 0x05 } else { 0x06 };
            (vk, msg == WM_XBUTTONDOWN, msg == WM_XBUTTONUP)
        }
        _ => (0, false, false),
    }
}

fn install_hooks() {
    // Callbacks live in this process (not in a DLL), so hMod is null; dwThreadId
    // 0 makes them system-wide. They must be installed from a thread that pumps
    // messages, i.e. the Slint UI thread.
    let kb = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), None, 0) };
    let ms = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(ms_proc), None, 0) };
    let kb_err = format!("{:?}", kb.as_ref().err());
    let ms_err = format!("{:?}", ms.as_ref().err());
    KB_HOOK.store(kb.map(|h| h.0 as isize).unwrap_or(0), Ordering::Relaxed);
    MS_HOOK.store(ms.map(|h| h.0 as isize).unwrap_or(0), Ordering::Relaxed);
    crate::ntrace!(
        "install_hooks thread={:?} kb={}(err={}) ms={}(err={})",
        std::thread::current().id(),
        KB_HOOK.load(Ordering::Relaxed),
        kb_err,
        MS_HOOK.load(Ordering::Relaxed),
        ms_err
    );
}

/// Swap a fresh hook in. Windows drops a low-level hook whose callback overran
/// its timeout without telling anybody, so this runs periodically. Must be
/// called from the hook pump thread.
fn reinstall_hooks() {
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

/// Low-level hooks are delivered through the installing thread's message queue,
/// so the hook needs a thread that actually pumps one. This thread does nothing
/// else: no windows, no callbacks that could block, just GetMessage.
fn start_hook_thread() {
    std::thread::spawn(|| unsafe {
        // Force this thread's message queue into existence before the hook goes
        // in, then install and start pumping.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        HOOK_THREAD_ID.store(GetCurrentThreadId(), Ordering::SeqCst);
        install_hooks();
        crate::ntrace!(
            "hook thread pumping tid={} kb={} ms={}",
            GetCurrentThreadId(),
            KB_HOOK.load(Ordering::Relaxed),
            MS_HOOK.load(Ordering::Relaxed)
        );

        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if ret.0 <= 0 {
                break;
            }
            if msg.message == WM_APP + 1 {
                reinstall_hooks();
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        crate::ntrace!("hook thread stopped");
    });
}

// ----- polling thread -----

fn apply_chord(hotkey: &Hotkey) {
    let mut bits = 0;
    if hotkey.ctrl {
        bits |= M_CTRL;
    }
    if hotkey.alt {
        bits |= M_ALT;
    }
    if hotkey.shift {
        bits |= M_SHIFT;
    }
    if hotkey.win {
        bits |= M_WIN;
    }
    MODS.store(bits, Ordering::SeqCst);
    KEY_VK.store(if hotkey.mouse != 0 { 0 } else { hotkey.key }, Ordering::SeqCst);
    MOUSE_VK.store(hotkey.mouse, Ordering::SeqCst);
    HELD.store(0, Ordering::SeqCst);
    SUPPRESS.store(0, Ordering::SeqCst);
}

/// Single background thread that owns both the trigger edges and the rebinding
/// edge detector. Polling is deliberate: GetAsyncKeyState keeps working when
/// Windows has silently dropped a low-level hook, which is the case that used
/// to make a resident hotkey look dead with no way to tell.
fn start_poller() {
    std::thread::spawn(|| {
        const ESC: u32 = 0x1B;
        const CAPTURE_TIMEOUT: Duration = Duration::from_secs(15);

        let mut prev = [false; 256];
        let mut seeded = false;
        let mut ticks: u32 = 0;
        // Hook liveness: the callbacks bump a heartbeat; one that stays still for
        // ALIVE_TICKS is not being called and the poller has to take over.
        const ALIVE_TICKS: u32 = 375;
        let mut kb_beat = KB_BEAT.load(Ordering::Relaxed);
        let mut ms_beat = MS_BEAT.load(Ordering::Relaxed);
        let mut kb_idle: u32 = ALIVE_TICKS;
        let mut ms_idle: u32 = ALIVE_TICKS;
        let mut poller_pressed = false;
        let mut engaged_ticks: u32 = 0;
        let mut seq = 0u64;
        let mut capture_started = Instant::now();

        loop {
            std::thread::sleep(Duration::from_millis(8));

            if CAPTURING.load(Ordering::SeqCst) == 1 {
                let now_seq = CAPTURE_SEQ.load(Ordering::SeqCst);
                if !seeded || now_seq != seq {
                    // Seed with whatever is already held: the click that started
                    // the capture must not become the new hotkey.
                    for vk in 0..256usize {
                        prev[vk] = key_down(vk as u32);
                    }
                    seeded = true;
                    seq = now_seq;
                    capture_started = Instant::now();
                    continue;
                }
                if capture_started.elapsed() > CAPTURE_TIMEOUT {
                    CAPTURING.store(0, Ordering::SeqCst);
                    emit(HotkeyEvent::CaptureCancelled);
                    continue;
                }
                let mods = mods_now();
                let mut hit: Option<Result<Hotkey, ()>> = None;
                for vk in 1u32..=254 {
                    let is_down = key_down(vk);
                    let was = prev[vk as usize];
                    prev[vk as usize] = is_down;
                    if !is_down || was || hit.is_some() {
                        continue;
                    }
                    if vk == ESC {
                        hit = Some(Err(()));
                        continue;
                    }
                    if let Some(hk) = classify(vk, mods) {
                        hit = Some(Ok(hk));
                        continue;
                    }
                    // Not usable: say so and keep waiting, so a mistyped chord is
                    // not silently ignored.
                    if let Some(code) = reject_reason(vk, mods) {
                        crate::ntrace!("capture rejected vk={vk} mods={mods} code={code}");
                        emit(HotkeyEvent::CaptureRejected(code));
                    }
                }
                if let Some(result) = hit {
                    CAPTURING.store(0, Ordering::SeqCst);
                    match result {
                        Ok(hk) => {
                            STAT_CAPTURE_HITS.fetch_add(1, Ordering::Relaxed);
                            emit(HotkeyEvent::Captured(hk));
                        }
                        Err(()) => emit(HotkeyEvent::CaptureCancelled),
                    }
                }
                continue;
            }
            seeded = false;
            ticks = ticks.wrapping_add(1);

            let nkb = KB_BEAT.load(Ordering::Relaxed);
            if nkb != kb_beat {
                kb_beat = nkb;
                kb_idle = 0;
            } else {
                kb_idle = kb_idle.saturating_add(1);
            }
            let nms = MS_BEAT.load(Ordering::Relaxed);
            if nms != ms_beat {
                ms_beat = nms;
                ms_idle = 0;
            } else {
                ms_idle = ms_idle.saturating_add(1);
            }

            let mouse_chord = MOUSE_VK.load(Ordering::Relaxed) != 0;
            let hook_alive = if mouse_chord {
                ms_idle < ALIVE_TICKS
            } else {
                kb_idle < ALIVE_TICKS
            };

            // Last-resort guard: a hotkey-triggered overlay that somehow never
            // sees its release must not keep the keyboard. 30s is far longer than
            // any real hold, and the overlay is visible the whole time.
            if SWALLOW.load(Ordering::Relaxed) == 1 && HELD.load(Ordering::Relaxed) == 1 {
                engaged_ticks = engaged_ticks.saturating_add(1);
                if engaged_ticks > 3750 {
                    crate::ntrace!("engaged too long, forcing release");
                    engaged_ticks = 0;
                    release();
                }
            } else {
                engaged_ticks = 0;
            }

            if hook_alive {
                // The hook is swallowing the chord, so its key never reaches the
                // async key state and polling cannot see it. Stand down. Only
                // give back a chord the poller itself opened: a chord the hook
                // opened is the hook's to release, and it will see the key up.
                if poller_pressed {
                    poller_pressed = false;
                    release();
                }
                continue;
            }

            if ticks % 500 == 0 {
                crate::ntrace!(
                    "poller owns trigger key={} mouse={} mods={} down={} held={}",
                    KEY_VK.load(Ordering::Relaxed),
                    MOUSE_VK.load(Ordering::Relaxed),
                    MODS.load(Ordering::Relaxed),
                    chord_down(),
                    HELD.load(Ordering::Relaxed)
                );
            }
            if chord_down() {
                // Suppression only guards this path: right after a rebind the
                // chord's own keys are still held, and polling would otherwise
                // read that as a fresh press. A real new key press always goes
                // through the hook, which is never suppressed.
                if SUPPRESS.load(Ordering::Relaxed) == 0 {
                    if HELD.load(Ordering::Relaxed) == 0 {
                        poller_pressed = true;
                    }
                    press();
                }
            } else {
                // The physical state is authoritative while the poller owns the
                // trigger, so any engaged chord is finished here - including one
                // the hook opened and then lost sight of. This also clears a
                // rebind suppression.
                poller_pressed = false;
                release();
            }
        }
    });
}

// ----- public API -----

/// Install the hooks and start the poller. Must be called from the thread that
/// runs the Slint event loop.
pub fn install(tx: Sender<HotkeyEvent>, hotkey: Hotkey) {
    let _ = EVENT_TX.set(tx);
    apply_chord(&hotkey);
    start_hook_thread();
    start_poller();
    crate::ntrace!("installed: chord={hotkey:?}");
}

/// Toggle rebinding capture mode. Capture itself never swallows input: the
/// physical key state is what gets recorded, so the user keeps typing into
/// whatever window is focused (which, when they just clicked "改绑", is ours).
pub fn set_capturing(on: bool) {
    CAPTURING.store(if on { 1 } else { 0 }, Ordering::SeqCst);
    CAPTURE_SEQ.fetch_add(1, Ordering::SeqCst);
    // Cancelling a rebind must leave the next press working; only a completed
    // rebind (set_hotkey) needs the suppression, because only then are the
    // chord's own keys still held.
    SUPPRESS.store(0, Ordering::SeqCst);
    crate::ntrace!("set_capturing({on})");
}

/// Ask the hook thread to swap its hooks for fresh ones. Windows drops a hook
/// whose callback overran its timeout without telling anybody.
pub fn request_hook_refresh() {
    let tid = HOOK_THREAD_ID.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                tid,
                WM_APP + 1,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

/// Replace the active chord after a rebind.
pub fn set_hotkey(hotkey: Hotkey) {
    apply_chord(&hotkey);
    SUPPRESS.store(1, Ordering::SeqCst);
    crate::ntrace!("set_hotkey({hotkey:?})");
}

/// Tell the hotkey layer whether the overlay is currently on screen because of
/// the hotkey. Only then is the keyboard taken away from the foreground
/// application, and the flag lives exactly as long as the overlay does.
pub fn set_overlay_engaged(on: bool) {
    SWALLOW.store(if on { 1 } else { 0 }, Ordering::SeqCst);
    if !on {
        SUPPRESS.store(0, Ordering::SeqCst);
    }
    crate::ntrace!("overlay engaged={on}");
}

/// Offline self-test of the pure chord logic. No OS input involved.
#[cfg(debug_assertions)]
pub fn selftest() -> Vec<(String, bool)> {
    let ctrl_alt_n = Hotkey {
        ctrl: true,
        alt: true,
        shift: false,
        win: false,
        key: 0x4E,
        mouse: 0,
    };
    vec![
        ("bare letter is rejected".into(), classify(0x4E, 0).is_none()),
        (
            "ctrl+alt+letter is accepted".into(),
            classify(0x4E, M_CTRL | M_ALT) == Some(ctrl_alt_n),
        ),
        (
            "modifier keys are never a chord".into(),
            classify(0x11, M_CTRL).is_none() && classify(0x5B, 0).is_none(),
        ),
        ("bare function key is accepted".into(), classify(0x70, 0).is_some()),
        (
            "left/right mouse stay free".into(),
            classify(0x01, M_CTRL).is_none() && classify(0x02, M_CTRL).is_none(),
        ),
        (
            "side mouse button is a chord".into(),
            classify(0x05, M_CTRL).map(|h| h.mouse == 0x05 && h.key == 0) == Some(true),
        ),
        (
            "shift alone does not qualify a letter".into(),
            classify(0x4E, M_SHIFT).is_none(),
        ),
        ("wheel delta decode".into(), {
            let info = MSLLHOOKSTRUCT {
                mouseData: 120u32 << 16,
                ..Default::default()
            };
            let delta = ((info.mouseData >> 16) & 0xFFFF) as i16 as i32;
            delta == 120
        }),
    ]
}
