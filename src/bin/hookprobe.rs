//! Diagnostic probe: does this process receive low-level input at all?
//!
//! Logs keyboard + mouse hook events, the session-wide last-input tick, and the
//! foreground window, so we can tell apart:
//!   * hooks globally blocked (other software eats them / policy)
//!   * keyboard hooks only blocked
//!   * input never reaching this desktop session
//!
//! Run: cargo run --bin hookprobe

use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowTextW,
    GetWindowThreadProcessId, KBDLLHOOKSTRUCT, MSLLHOOKSTRUCT, PeekMessageW, SetWindowsHookExW,
    TranslateMessage, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP, MSG,
    PM_NOREMOVE,
};

static KBD: AtomicU32 = AtomicU32::new(0);
static MSE: AtomicU32 = AtomicU32::new(0);
static MOV: AtomicU32 = AtomicU32::new(0);

fn log(line: &str) {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let p = std::path::PathBuf::from(base).join("NoKeys").join("probe.log");
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    }
}

unsafe extern "system" fn kb(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = w.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if down || up {
            let info = &*(l.0 as *const KBDLLHOOKSTRUCT);
            KBD.fetch_add(1, Ordering::Relaxed);
            log(&format!(
                "KB {} vk=0x{:02X} flags=0x{:X}",
                if down { "DOWN" } else { "UP" },
                info.vkCode,
                info.flags.0
            ));
        }
    }
    unsafe { CallNextHookEx(None, code, w, l) }
}

unsafe extern "system" fn ms(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = w.0 as u32;
        match msg {
            WM_MOUSEMOVE => {
                MOV.fetch_add(1, Ordering::Relaxed);
            }
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_MOUSEWHEEL => {
                MSE.fetch_add(1, Ordering::Relaxed);
                let info = &*(l.0 as *const MSLLHOOKSTRUCT);
                log(&format!(
                    "MS msg=0x{:X} pt=({},{}) data=0x{:X}",
                    msg, info.pt.x, info.pt.y, info.mouseData
                ));
            }
            _ => {}
        }
    }
    unsafe { CallNextHookEx(None, code, w, l) }
}

fn foreground_info() -> String {
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.0.is_null() {
            return "none".into();
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        format!("pid={pid} title=\"{title}\"")
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetTickCount() -> u32;
}
unsafe fn get_tick_count() -> u32 {
    unsafe { GetTickCount() }
}

fn last_input_age_ms() -> u32 {
    unsafe {
        let mut li = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut li).as_bool() {
            let now = get_tick_count();
            now.wrapping_sub(li.dwTime)
        } else {
            u32::MAX
        }
    }
}

fn main() {
    log(&format!("=== probe start pid={} ===", std::process::id()));

    let h = std::thread::spawn(|| unsafe {
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);

        let kb_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb), None, 0);
        let ms_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(ms), None, 0);
        log(&format!(
            "hooks installed kb={} ms={}",
            kb_hook.is_ok(),
            ms_hook.is_ok()
        ));

        // Session-wide input heartbeat, independent of any hook.
        std::thread::spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            log(&format!(
                "STAT kbd={} mouse_btn={} mouse_move={} last_input={}ms_ago fg[{}]",
                KBD.load(Ordering::Relaxed),
                MSE.load(Ordering::Relaxed),
                MOV.load(Ordering::Relaxed),
                last_input_age_ms(),
                foreground_info()
            ));
        });

        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if ret.0 <= 0 {
                log(&format!("loop exit {}", ret.0));
                break;
            }
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    });
    let _ = h.join();
}
