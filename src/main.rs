#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backdrop;
mod config;
mod settings_ui;
#[macro_use]
mod trace;
mod hotkey;

slint::include_modules!();

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use config::{Config, Item, Route};
use hotkey::HotkeyEvent;
use settings_ui::{refresh_overlay_style, sync_settings, wire_settings};
use slint::{
    ComponentHandle, Image, ModelRc, PhysicalPosition, PhysicalSize, SharedString, VecModel, Color,
};

// ----- layout constants (logical pixels) -----
const WHEEL_W: f32 = 480.0;
const WHEEL_H: f32 = 520.0;
const DISC_X: f32 = 40.0;
const DISC_Y: f32 = 40.0;
const DISC_D: f32 = 400.0;
const DISC_R: f32 = DISC_D / 2.0;
const DISC_CX: f32 = DISC_X + DISC_R; // 240
const DISC_CY: f32 = DISC_Y + DISC_R; // 240
const CONTENT_R: f32 = 134.0;
const HUB_R: f32 = 30.0;

// List view: a grid of 4:3 cards, icons in the middle, title underneath.
const LIST_MARGIN: f32 = 28.0;
const LIST_CARD_W: f32 = 120.0;
const LIST_CARD_H: f32 = 90.0;
const LIST_GAP: f32 = 14.0;
const LIST_COLS_MIN: u32 = 2;
const LIST_COLS_MAX: u32 = 8;

/// Bumped on every open/close so a delayed timer can tell whether it is still
/// talking about the same overlay session.
static OVERLAY_GEN: AtomicU64 = AtomicU64::new(0);

// ----- app state (only touched on the UI thread) -----
struct AppState {
    config: Config,
    current_wheel: usize,
    hovered: i32,
    open: bool,
    view_is_wheel: bool,
    /// ids of the items shown in the list view, in order.
    list_ids: Vec<u32>,
    /// settings dialog is waiting for a hotkey chord
    capturing: bool,
    /// wheel selected in the settings dialog
    selected_wheel: usize,
    /// device-pixel top-left of the overlay window, for cursor hit-testing
    overlay_origin: (i32, i32),
    /// device pixels per logical pixel for the overlay window
    overlay_scale: f32,
    /// cursor position when the overlay opened; the overlay only activates an
    /// item once the pointer actually moved away from it
    open_cursor: (i32, i32),
}

fn main() -> Result<(), slint::PlatformError> {
    // The software renderer keeps resident memory ~3.5x lower than the OpenGL
    // (femtovg) path, which matters for an always-resident utility.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        std::env::set_var("SLINT_BACKEND", "winit-software");
    }

    if std::env::var("NOKEYS_SELFTEST").is_ok() {
        #[cfg(debug_assertions)]
        {
            for (step, ok) in hotkey::selftest() {
                println!("{} {step}", if ok { "PASS" } else { "FAIL" });
            }
            for (step, ok) in config::selftest() {
                println!("{} {step}", if ok { "PASS" } else { "FAIL" });
            }
        }
        return Ok(());
    }

    if std::env::var("NOKEYS_ALLOW_MULTI").is_err() && !claim_single_instance() {
        focus_existing_instance();
        return Ok(());
    }

    let mut config = config::load();
    config.ensure_list_entry();
    config.normalize_vks();

    let overlay = OverlayWindow::new()?;
    let settings = SettingsWindow::new()?;
    // The tray is a separate component with no window; it must stay alive for the
    // whole process, which it does by being owned here.
    let tray = TrayIcon::new()?;

    let state = Arc::new(Mutex::new(AppState {
        current_wheel: 0,
        hovered: -1,
        open: false,
        view_is_wheel: true,
        list_ids: Vec::new(),
        capturing: false,
        selected_wheel: 0,
        overlay_origin: (0, 0),
        overlay_scale: 1.0,
        open_cursor: (0, 0),
        config,
    }));

    // initial theme + animation + models
    {
        let mut st = state.lock().unwrap();
        apply_theme(&overlay);
        apply_animation(&overlay, &st.config);
        overlay.set_show_wheel(true);
        overlay.set_wheel_index(1);
        overlay.set_wheel_total(st.config.wheels.len() as i32);
        overlay.set_reveal(false);
        overlay.set_wheel_swap(true);
        rebuild_wheel_model(&overlay, &st);
        rebuild_list_model(&overlay, &mut st);
    }

    // ---- overlay callbacks ----
    let state_hover = state.clone();
    let overlay_weak_hover = overlay.as_weak();
    overlay.on_hover_at(move |x: f32, y: f32| {
        if let Some(ov) = overlay_weak_hover.upgrade() {
            let st = state_hover.lock().unwrap();
            let idx = if st.view_is_wheel {
                wheel_sector_at(x, y, &st)
            } else {
                grid_index_at(x, y, &st)
            };
            drop(st);
            ov.set_hovered(idx);
        }
    });

    let state_click = state.clone();
    let overlay_weak_click = overlay.as_weak();
    overlay.on_click_at(move |x: f32, y: f32| {
        if let Some(ov) = overlay_weak_click.upgrade() {
            let id = {
                let st = state_click.lock().unwrap();
                if st.view_is_wheel {
                    let idx = wheel_sector_at(x, y, &st);
                    if idx >= 0 {
                        Some(st.config.wheels[st.current_wheel].items[idx as usize].id as i32)
                    } else {
                        None
                    }
                } else {
                    grid_index_at(x, y, &st)
                        .try_into()
                        .ok()
                        .and_then(|i: usize| st.list_ids.get(i).copied())
                        .map(|id| id as i32)
                }
            };
            if let Some(id) = id {
                activate_and_close(&ov, &state_click, id);
            }
        }
    });

    let state_activate = state.clone();
    let overlay_weak_activate = overlay.as_weak();
    overlay.on_activate(move |id: i32| {
        if let Some(ov) = overlay_weak_activate.upgrade() {
            activate_and_close(&ov, &state_activate, id);
        }
    });

    let state_close = state.clone();
    let overlay_weak_close = overlay.as_weak();
    overlay.on_close_requested(move || {
        if let Some(ov) = overlay_weak_close.upgrade() {
            let mut st = state_close.lock().unwrap();
            close_overlay(&ov, &mut st);
        }
    });

    // ---- settings: save config on close ----
    let state_save = state.clone();
    settings.window().on_close_requested(move || {
        let st = state_save.lock().unwrap();
        let _ = config::save(&st.config);
        slint::CloseRequestResponse::HideWindow
    });

    // ---- settings: hotkey label + preview buttons ----
    settings.set_hotkey_label(hotkey_label(&state.lock().unwrap().config.hotkey).into());

    let state_pw = state.clone();
    let overlay_weak_pw = overlay.as_weak();
    settings.on_preview_wheel(move || {
        if let Some(ov) = overlay_weak_pw.upgrade() {
            let mut st = state_pw.lock().unwrap();
            let idx = st.current_wheel.min(st.config.wheels.len().saturating_sub(1));
            open_overlay_view(&ov, &mut st, true, idx, false);
        }
    });

    let state_pl = state.clone();
    let overlay_weak_pl = overlay.as_weak();
    settings.on_preview_list(move || {
        if let Some(ov) = overlay_weak_pl.upgrade() {
            let mut st = state_pl.lock().unwrap();
            let idx = st.current_wheel.min(st.config.wheels.len().saturating_sub(1));
            open_overlay_view(&ov, &mut st, false, idx, false);
        }
    });

    // ---- settings: quit ----
    let state_quit = state.clone();
    settings.on_quit_app(move || {
        config::save(&state_quit.lock().unwrap().config).ok();
        let _ = slint::quit_event_loop();
    });

    if let Ok(page) = std::env::var("NOKEYS_PAGE") {
        if let Ok(p) = page.parse::<i32>() {
            settings.set_page(p);
        }
    }

    // ---- tray: reopen the settings, or quit ----
    {
        let settings_weak = settings.as_weak();
        let open_settings = move || {
            if let Some(se) = settings_weak.upgrade() {
                if se.show().is_ok() {
                    force_foreground_settings();
                }
            }
        };
        tray.on_open_settings(open_settings);
        let state_quit = state.clone();
        tray.on_quit_app(move || {
            config::save(&state_quit.lock().unwrap().config).ok();
            let _ = slint::quit_event_loop();
        });
    }

    ntrace!("init: windows + callbacks ready");
    wire_settings(&settings, &overlay, &state);
    ntrace!("init: settings wired");
    sync_settings(&settings, &state);
    ntrace!("init: settings synced");
    refresh_overlay_style(&overlay, &state);
    ntrace!("init: overlay style refreshed");

    // ---- global hotkey (installed on the UI thread; the Slint loop pumps messages) ----
    let (tx, rx) = mpsc::channel::<HotkeyEvent>();
    {
        let st = state.lock().unwrap();
        hotkey::install(tx, st.config.hotkey.clone());
    }

    // ---- event dispatch thread ----
    let overlay_weak = overlay.as_weak();
    let settings_weak = settings.as_weak();
    let state_events = state.clone();
    std::thread::spawn(move || {
        for ev in rx {
            let ow = overlay_weak.clone();
            let sw = settings_weak.clone();
            let st = state_events.clone();
            ntrace!("rx ev={ev:?}");
            let res = slint::invoke_from_event_loop(move || {
                let ov = ow.upgrade();
                let se = sw.upgrade();
                if matches!(ev, HotkeyEvent::CaptureCancelled) {
                    ntrace!("capture cancelled");
                    hotkey::set_capturing(false);
                    if let Some(se) = &se {
                        st.lock().unwrap().capturing = false;
                        se.set_capture_hint("".into());
                        sync_settings(se, &st);
                    }
                    return;
                }
                if let HotkeyEvent::CaptureRejected(code) = ev {
                    if let Some(se) = &se {
                        let msg = match code {
                            1 => "字母/数字需配 Ctrl·Alt·Win；单键用 F1–F24、媒体键或鼠标侧键",
                            _ => "左右键留给普通点击，单键可用鼠标侧键或中键",
                        };
                        se.set_capture_hint(msg.into());
                    }
                    return;
                }
                if let HotkeyEvent::Captured(hk) = ev {
                    // Leaving capture mode must never depend on the settings
                    // window still being alive: a missed clear here is what used
                    // to leave the machine with a dead keyboard.
                    hotkey::set_capturing(false);
                    if let Some(se) = &se {
                        let mut state = st.lock().unwrap();
                        ntrace!("captured hk={hk:?} capturing={}", state.capturing);
                        if state.capturing {
                            state.capturing = false;
                            state.config.hotkey = hk;
                            hotkey::set_hotkey(hk);
                            config::save(&state.config).ok();
                            drop(state);
                            se.set_capture_hint("".into());
                            sync_settings(se, &st);
                            // Flash the field so an accepted rebound is visible even
                            // when the chord did not change.
                            let weak = se.as_weak();
                            se.set_bind_flash(true);
                            slint::Timer::single_shot(std::time::Duration::from_millis(1400), move || {
                                if let Some(se) = weak.upgrade() {
                                    se.set_bind_flash(false);
                                }
                            });
                        }
                    }
                    return;
                }
                if let Some(ov) = ov {
                    handle_event(ev, &ov, &st);
                }
            });
            if let Err(e) = res {
                ntrace!("invoke_from_event_loop failed: {e}");
            }
        }
    });

    // Dev harness: NOKEYS_AUTOPEN=wheel|list auto-opens after 2s.
    if std::env::var("NOKEYS_AUTOPEN").is_ok() {
        let state_a = state.clone();
        let ow_a = overlay.as_weak();
        let as_list = std::env::var("NOKEYS_AUTOPEN").unwrap_or_default() == "list";
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let ow_open = ow_a.clone();
            let state_open = state_a.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ov) = ow_open.upgrade() {
                    let mut st = state_open.lock().unwrap();
                    let idx = st.current_wheel.min(st.config.wheels.len().saturating_sub(1));
                    open_overlay_view(&ov, &mut st, !as_list, idx, false);
                    if let Ok(h) = std::env::var("NOKEYS_HOVER") {
                        if let Ok(h) = h.parse::<i32>() {
                            ov.set_hovered(h);
                            st.hovered = h;
                        }
                    }
                }
            });
        });
    }

    // Dev harness: NOKEYS_RUN_ITEM=<id> runs that item's action once, 2.5s after
    // startup, without touching the overlay. Used to test the executors.
    if let Ok(id) = std::env::var("NOKEYS_RUN_ITEM") {
        if let Ok(id) = id.parse::<i32>() {
            let st = state.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(2500));
                let _ = slint::invoke_from_event_loop(move || {
                    let s = st.lock().unwrap();
                    ntrace!("dev harness: run item {id}");
                    activate_id(&s.config, id);
                });
            });
        }
    }

    // Diagnostic: hook liveness from a plain thread (never from the hook).
    if std::env::var("NOKEYS_TRACE").is_ok() {
        std::thread::spawn(|| {
            let mut last = (u32::MAX, u32::MAX, u32::MAX, u32::MAX);
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let now = (
                    hotkey::stat_hook_rounds(),
                    hotkey::stat_capture_active(),
                    hotkey::stat_capture_hits(),
                    hotkey::stat_keys(),
                );
                if now != last {
                    ntrace!(
                        "hotkey stats beats={} cap={} hits={} keys={} mouse={} swallowed={}",
                        now.0,
                        now.1,
                        now.2,
                        now.3,
                        hotkey::stat_mouse(),
                        hotkey::stat_swallowed()
                    );
                    last = now;
                }
            }
        });
    }

    // Refresh the low-level hooks periodically: Windows removes a hook whose
    // callback overran its timeout without telling anybody.
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(30));
        hotkey::request_hook_refresh();
    });

    ntrace!("init: showing settings window");
    settings.show()?;
    tray.show()?;
    ntrace!("init: tray visible");
    ntrace!("init: settings visible, entering event loop");
    // run_event_loop() would terminate as soon as the settings window is hidden.
    // This program is resident by design: the hotkey has to keep working with no
    // window on screen, so the loop must survive having nothing visible.
    slint::run_event_loop_until_quit()?;
    Ok(())
}

// ================= helpers =================

/// Bring the settings window to the front after showing it from the tray.
fn force_foreground_settings() {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow};
    if let Ok(hwnd) = unsafe { FindWindowW(None, w!("No Keys - 设置")) } {
        if !hwnd.is_invalid() {
            unsafe {
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}

fn apply_theme(ov: &OverlayWindow) {
    // The overlay is always dark: colour customisation was removed on purpose.
    // No panel behind the wheel: it floats on the desktop and only the
    // frosted snapshot (when enabled) gives it a body.
    ov.set_disc_tint(Color::from_argb_u8(0x00, 0x00, 0x00, 0x00));
    ov.set_disc_tint_2(Color::from_argb_u8(0x00, 0x00, 0x00, 0x00));
    ov.set_blur_tint(Color::from_argb_u8(0x2e, 0x1a, 0x1f, 0x2b));
    ov.set_blur_tint_2(Color::from_argb_u8(0x55, 0x0a, 0x0c, 0x11));
    ov.set_disc_border(Color::from_argb_u8(0x33, 0xff, 0xff, 0xff));
    ov.set_sector_stroke(Color::from_argb_u8(0x2a, 0xff, 0xff, 0xff));
    // Selection is an outline, never a fill: nothing may be painted behind
    // the items or inside the wheel.
    ov.set_sector_hover(Color::from_argb_u8(0x00, 0x5b, 0x8c, 0xff));
    ov.set_text_color(Color::from_argb_u8(0xff, 0xf2, 0xf4, 0xf8));
    ov.set_card_bg(Color::from_argb_u8(0xe0, 0x1b, 0x1e, 0x27));
    ov.set_card_hover_bg(Color::from_argb_u8(0xf2, 0x25, 0x2a, 0x38));
    ov.set_sub_text_color(Color::from_argb_u8(0xa8, 0xf2, 0xf4, 0xf8));
    ov.set_close_bg(Color::from_argb_u8(0x2e, 0xff, 0xff, 0xff));
    ov.set_close_hover_bg(Color::from_argb_u8(0xdd, 0xe5, 0x48, 0x4d));
    ov.set_icon_ph_bg(Color::from_argb_u8(0x1a, 0xff, 0xff, 0xff));
    ov.set_icon_ph_line(Color::from_argb_u8(0x73, 0xff, 0xff, 0xff));
    ov.set_accent(Color::from_argb_u8(0xff, 0x5b, 0x8c, 0xff));
    ov.set_dot_bg(Color::from_argb_u8(0x4d, 0xff, 0xff, 0xff));
}

/// Effective length of one animation: the base length divided by the global
/// speed and the animation's own multiplier.
fn anim_ms(config: &Config, base: i64, mult: f32) -> u64 {
    let a = &config.animation;
    let s = (a.global_speed * mult).max(0.05);
    ((base as f32 / s).round().max(1.0)) as u64
}

fn apply_animation(ov: &OverlayWindow, config: &Config) {
    let a = &config.animation;
    ov.set_fade_dur(anim_ms(config, 150, a.fade_mult) as i64);
    ov.set_slide_dur(anim_ms(config, 220, a.slide_mult) as i64);
    ov.set_hover_dur(anim_ms(config, 130, a.hover_fade_mult.min(a.hover_scale_mult)) as i64);
    ov.set_stagger_dur(anim_ms(config, 45, a.stagger_mult) as i64);
}

fn load_icon(path: &str) -> Image {
    if path.is_empty() {
        return Image::default();
    }
    Image::load_from_path(std::path::Path::new(path)).unwrap_or_default()
}

fn rebuild_wheel_model(ov: &OverlayWindow, st: &AppState) {
    let items: &Vec<Item> = &st.config.wheels[st.current_wheel].items;
    let n = items.len();
    let mut model: Vec<WheelItem> = Vec::with_capacity(n);
    for (i, it) in items.iter().enumerate() {
        let (cx, cy) = content_pos(n, i);
        let icon = load_icon(&it.icon);
        let has_icon = !it.icon.is_empty() && icon.size().width > 0;
        model.push(WheelItem {
            id: it.id as i32,
            name: SharedString::from(it.name.as_str()),
            icon,
            has_icon,
            is_list: it.is_list_entry,
            path: SharedString::from(wedge_path(n, i)),
            cx,
            cy,
        });
    }
    ov.set_wheel_items(ModelRc::new(VecModel::from(model)));
}

fn list_columns(config: &Config) -> u32 {
    config.list_columns.clamp(LIST_COLS_MIN, LIST_COLS_MAX)
}

fn rebuild_list_model(ov: &OverlayWindow, st: &mut AppState) {
    let mut model: Vec<ListItem> = Vec::new();
    let mut ids: Vec<u32> = Vec::new();
    let cols = list_columns(&st.config) as usize;
    ov.set_list_card_w(LIST_CARD_W);
    ov.set_list_card_h(LIST_CARD_H);
    for wheel in &st.config.wheels {
        for it in &wheel.items {
            if it.is_list_entry {
                continue;
            }
            let idx = ids.len();
            let (col, row) = (idx % cols, idx / cols);
            let x = LIST_MARGIN + col as f32 * (LIST_CARD_W + LIST_GAP);
            let y = LIST_MARGIN + row as f32 * (LIST_CARD_H + LIST_GAP);
            let icon = load_icon(&it.icon);
            let has_icon = !it.icon.is_empty() && icon.size().width > 0;
            model.push(ListItem {
                id: it.id as i32,
                name: SharedString::from(it.name.as_str()),
                icon,
                has_icon,
                x,
                y,
            });
            ids.push(it.id);
        }
    }
    st.list_ids = ids;
    ov.set_list_items(ModelRc::new(VecModel::from(model)));
}

/// Outer radius of a sector, in the 400x400 pixel space of the disc box.
const SECTOR_R_OUT: f32 = DISC_D / 2.0 * 0.985;
/// Inner radius of a sector. Sectors are rings, not pies: nothing is painted
/// behind the centre hub, so the hub can stay translucent without the sector
/// edges and dividers showing through the wheel number.
const SECTOR_R_IN: f32 = 32.0;

/// Ring sector path in the 400x400 pixel space of the disc box.
fn wedge_path(n: usize, i: usize) -> String {
    if n < 2 {
        return String::new();
    }
    let (cx, cy) = ((DISC_D / 2.0) as f64, (DISC_D / 2.0) as f64);
    let gap = 2.4;
    let start = -90.0 + i as f64 * (360.0 / n as f64) + gap / 2.0;
    let end = -90.0 + (i + 1) as f64 * (360.0 / n as f64) - gap / 2.0;
    let ro = SECTOR_R_OUT as f64;
    let ri = SECTOR_R_IN as f64;
    let (x0, y0) = polar(cx, cy, ro, start);
    let (x1, y1) = polar(cx, cy, ro, end);
    let (x2, y2) = polar(cx, cy, ri, end);
    let (x3, y3) = polar(cx, cy, ri, start);
    format!(
        "M {x0:.3} {y0:.3} A {ro} {ro} 0 0 1 {x1:.3} {y1:.3} L {x2:.3} {y2:.3} A {ri} {ri} 0 0 0 {x3:.3} {y3:.3} Z"
    )
}

fn content_pos(n: usize, i: usize) -> (f32, f32) {
    let mid = if n <= 1 {
        -90.0
    } else {
        -90.0 + (i as f64 + 0.5) * (360.0 / n as f64)
    };
    let rad = mid.to_radians();
    (
        DISC_CX + CONTENT_R * rad.cos() as f32,
        DISC_CY + CONTENT_R * rad.sin() as f32,
    )
}

fn polar(cx: f64, cy: f64, r: f64, deg: f64) -> (f64, f64) {
    let rad = deg.to_radians();
    (cx + r * rad.cos(), cy + r * rad.sin())
}

/// Grid position of a point in the list view, as a card index.
fn grid_index_at(x: f32, y: f32, st: &AppState) -> i32 {
    let cols = list_columns(&st.config) as usize;
    let step_x = LIST_CARD_W + LIST_GAP;
    let step_y = LIST_CARD_H + LIST_GAP;
    let col_f = (x - LIST_MARGIN) / step_x;
    let row_f = (y - LIST_MARGIN) / step_y;
    if col_f < 0.0 || row_f < 0.0 {
        return -1;
    }
    let (col, row) = (col_f as usize, row_f as usize);
    if col >= cols {
        return -1;
    }
    if x - LIST_MARGIN - col as f32 * step_x > LIST_CARD_W
        || y - LIST_MARGIN - row as f32 * step_y > LIST_CARD_H
    {
        return -1;
    }
    let idx = row * cols + col;
    if idx >= st.list_ids.len() {
        return -1;
    }
    idx as i32
}

fn wheel_sector_at(x: f32, y: f32, st: &AppState) -> i32 {
    let items = &st.config.wheels[st.current_wheel].items;
    let n = items.len();
    if n == 0 {
        return -1;
    }
    let dx = x - DISC_CX;
    let dy = y - DISC_CY;
    let dist = (dx * dx + dy * dy).sqrt();
    if dist > DISC_R || dist < HUB_R {
        return -1;
    }
    let ang = dy.atan2(dx).to_degrees();
    let norm = (ang + 90.0).rem_euclid(360.0);
    let idx = ((norm / 360.0) * n as f32) as usize % n;
    idx as i32
}

// ================= event handling =================

fn handle_event(ev: HotkeyEvent, ov: &OverlayWindow, state: &Arc<Mutex<AppState>>) {
    let mut st = state.lock().unwrap();
    match ev {
        HotkeyEvent::Down => {
            if st.open {
                // The list view is sticky: nothing is held, so a fresh press
                // dismisses it instead of doing nothing.
                if !st.view_is_wheel {
                    ntrace!("trigger down while list is open -> dismiss");
                    close_overlay(ov, &mut st);
                }
                return;
            }
            ntrace!("trigger down");
            open_overlay(ov, &mut st);
        }
        HotkeyEvent::Up => {
            if !st.open {
                return;
            }
            // Releasing without moving means "cancel": the wheel is anchored at
            // the cursor, and when it had to be clamped to stay on screen the
            // pointer no longer sits on the centre either.
            let cursor = get_cursor_pos();
            let (ox, oy) = st.open_cursor;
            let moved = (cursor.0 - ox).abs().max((cursor.1 - oy).abs()) >= 10;
            let id = if moved { item_at_cursor(&st) } else { None };
            ntrace!("trigger up -> {:?} (moved={moved})", id);
            // Releasing over an item runs it; over nothing it just closes, and
            // over the "list view" entry it switches to the list instead.
            match id {
                Some(id) => {
                    drop(st);
                    activate_and_close(ov, state, id);
                }
                None => close_overlay(ov, &mut st),
            }
        }
        HotkeyEvent::Wheel(delta) => {
            if !st.open || !st.view_is_wheel {
                return;
            }
            let n = st.config.wheels.len();
            if n <= 1 {
                return;
            }
            if delta > 0 {
                st.current_wheel = (st.current_wheel + n - 1) % n;
            } else {
                st.current_wheel = (st.current_wheel + 1) % n;
            }
            ov.set_wheel_index((st.current_wheel + 1) as i32);
            // Fade the old item ring out, swap the model, fade the new one in.
            ov.set_wheel_swap(false);
            rebuild_wheel_model(ov, &st);
            let ov_weak = ov.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(16), move || {
                if let Some(ov) = ov_weak.upgrade() {
                    ov.set_wheel_swap(true);
                }
            });
        }
        HotkeyEvent::Captured(_)
        | HotkeyEvent::CaptureCancelled
        | HotkeyEvent::CaptureRejected(_) => {
            // handled by the dispatch closure, which owns the settings window
        }
    }
}

/// Runs whatever the item is configured to do.
fn activate_id(config: &Config, id: i32) {
    let Some(item) = config
        .wheels
        .iter()
        .flat_map(|w| w.items.iter())
        .find(|it| it.id as i32 == id)
    else {
        ntrace!("activate {id}: no such item");
        return;
    };
    let action = item.action.clone();
    match &action {
        config::Action::Key { keys } if keys.trim().is_empty() => {
            // The item's own reserved key: another program can react to it and a
            // plain keyboard never produces it by accident.
            let Some(vk) = item.vk else {
                ntrace!("activate {id}: no reserved key assigned");
                return;
            };
            ntrace!(
                "activate nokeys_{id} -> {} for {}",
                config::vk_name(vk),
                foreground_window()
            );
            send_key(vk, true, false);
            std::thread::sleep(std::time::Duration::from_millis(20));
            send_key(vk, false, false);
        }
        config::Action::Key { keys } => match parse_chord(keys) {
            Some(chord) => {
                ntrace!(
                    "activate {id}: sending key {keys:?} to {}",
                    foreground_window()
                );
                send_chord(&chord);
            }
            None => ntrace!("activate {id}: cannot parse key {keys:?}"),
        },
        config::Action::Shortcut { keys } => match parse_chord(keys) {
            Some(chord) => {
                ntrace!(
                    "activate {id}: sending chord {keys:?} to {}",
                    foreground_window()
                );
                send_chord(&chord);
            }
            None => ntrace!("activate {id}: cannot parse chord {keys:?}"),
        },
        config::Action::Program { path, args } => {
            if path.trim().is_empty() {
                ntrace!("activate {id}: program with an empty path");
                return;
            }
            match shell_open(path, args) {
                Ok(()) => ntrace!("activate {id}: started {path:?} {args:?}"),
                Err(e) => ntrace!("activate {id}: program failed: {e}"),
            }
        }
        config::Action::Url { url } => {
            if url.trim().is_empty() {
                ntrace!("activate {id}: url is empty");
                return;
            }
            match shell_open(url, "") {
                Ok(()) => ntrace!("activate {id}: opened {url:?}"),
                Err(e) => ntrace!("activate {id}: url failed: {e}"),
            }
        }
        config::Action::Command { cmd } => {
            if cmd.trim().is_empty() {
                ntrace!("activate {id}: command is empty");
                return;
            }
            match run_hidden_command(cmd) {
                Ok(pid) => ntrace!("activate {id}: command started (pid {pid})"),
                Err(e) => ntrace!("activate {id}: command failed: {e}"),
            }
        }
        config::Action::Text { text } => {
            let expanded = expand_tokens(text);
            ntrace!("activate {id}: typing {expanded:?}");
            type_text(&expanded);
        }
    }
}

/// Describe the window that will receive the synthetic keys, so a report of
/// "nothing happened" can be traced back to the wrong target.
fn foreground_window() -> String {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return "none".into();
        }
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        let title = String::from_utf16_lossy(&buf[..n]);
        format!("{title:?} (pid {pid})")
    }
}

/// One key press or release through SendInput.
///
/// The event is built from the key's scan code whenever one exists: binding
/// dialogs (and games that read the hardware key stream) recognise scan-code
/// events far more reliably than virtual-key-only ones.
fn send_key(vk: u16, down: bool, extended: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC, MapVirtualKeyW,
        VIRTUAL_KEY,
    };
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    let (wvk, wscan, flags) = if scan != 0 {
        (VIRTUAL_KEY(0), scan, flags | KEYEVENTF_SCANCODE)
    } else {
        (VIRTUAL_KEY(vk), 0, flags)
    };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: wvk,
                wScan: wscan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Keys that need the extended-key flag, otherwise apps see the numpad variant.
fn is_extended_vk(vk: u16) -> bool {
    matches!(vk, 0x21..=0x2E | 0x5B | 0x5C | 0x6F | 0x90 | 0xA3 | 0xA5)
}

struct Chord {
    ctrl: bool,
    alt: bool,
    shift: bool,
    win: bool,
    vk: u16,
}

/// Parse chords like "Ctrl+Alt+M", "F8" or "Shift+Space".
fn parse_chord(text: &str) -> Option<Chord> {
    let mut chord = Chord { ctrl: false, alt: false, shift: false, win: false, vk: 0 };
    let mut found = false;
    for raw in text.split('+') {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => chord.ctrl = true,
            "alt" => chord.alt = true,
            "shift" => chord.shift = true,
            "win" | "meta" | "super" => chord.win = true,
            other => {
                chord.vk = vk_from_name(other)?;
                found = true;
            }
        }
    }
    if found {
        Some(chord)
    } else {
        None
    }
}

fn vk_from_name(name: &str) -> Option<u16> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        let upper = c.to_ascii_uppercase();
        if upper.is_ascii_alphanumeric() {
            return Some(upper as u16);
        }
        return match upper {
            ';' => Some(0xBA),
            '=' => Some(0xBB),
            ',' => Some(0xBC),
            '-' => Some(0xBD),
            '.' => Some(0xBE),
            '/' => Some(0xBF),
            '`' => Some(0xC0),
            '[' => Some(0xDB),
            '\\' => Some(0xDC),
            ']' => Some(0xDD),
            '\'' => Some(0xDE),
            _ => None,
        };
    }
    Some(match name {
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "backspace" | "bksp" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "capslock" | "caps" => 0x14,
        "printscreen" | "prtsc" => 0x2C,
        "pause" => 0x13,
        "plus" => 0xBB,
        "minus" => 0xBD,
        "comma" => 0xBC,
        "period" => 0xBE,
        "slash" => 0xBF,
        "semicolon" => 0xBA,
        "quote" => 0xDE,
        "grave" => 0xC0,
        "backslash" => 0xDC,
        _ => {
            if let Some(rest) = name.strip_prefix('f') {
                if let Ok(n) = rest.parse::<u16>() {
                    if (1..=24).contains(&n) {
                        return Some(0x70 + n - 1);
                    }
                }
            }
            if let Some(rest) = name.strip_prefix("num") {
                if let Ok(n) = rest.parse::<u16>() {
                    if n <= 9 {
                        return Some(0x60 + n);
                    }
                }
            }
            return None;
        }
    })
}

/// Press the modifiers, tap the key, release the modifiers.
fn send_chord(chord: &Chord) {
    let mut mods: Vec<u16> = Vec::new();
    if chord.ctrl {
        mods.push(0x11);
    }
    if chord.alt {
        mods.push(0x12);
    }
    if chord.shift {
        mods.push(0x10);
    }
    if chord.win {
        mods.push(0x5B);
    }
    for m in &mods {
        send_key(*m, true, false);
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    let ext = is_extended_vk(chord.vk);
    send_key(chord.vk, true, ext);
    std::thread::sleep(std::time::Duration::from_millis(20));
    send_key(chord.vk, false, ext);
    for m in mods.iter().rev() {
        std::thread::sleep(std::time::Duration::from_millis(8));
        send_key(*m, false, false);
    }
}

/// Hand a path or URL to the shell so .exe, .lnk, .bat and documents all work.
fn shell_open(target: &str, args: &str) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let verb = HSTRING::from("open");
    let file = HSTRING::from(target);
    let params = HSTRING::from(args);
    let params_ptr = if args.trim().is_empty() {
        windows::core::PCWSTR::null()
    } else {
        windows::core::PCWSTR(params.as_ptr())
    };
    let h = unsafe {
        ShellExecuteW(
            None,
            &verb,
            &file,
            params_ptr,
            None,
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a value > 32 on success.
    if h.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW error {}", h.0 as isize))
    }
}

/// Run a console command without flashing a console window.
fn run_hidden_command(cmd: &str) -> Result<u32, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // raw_arg, not arg(): std would quote and escape the whole command line,
    // and cmd.exe would then treat the inner quotes as literal characters, so a
    // redirect would silently create a file whose name contains quotes.
    std::process::Command::new("cmd")
        .raw_arg("/C")
        .raw_arg(cmd)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|c| c.id())
        .map_err(|e| e.to_string())
}

/// Local date, time and clipboard placeholders for the text action.
fn expand_tokens(text: &str) -> String {
    let (date, time) = local_date_time();
    let mut out = text.to_string();
    if out.contains("{date}") {
        out = out.replace("{date}", &date);
    }
    if out.contains("{time}") {
        out = out.replace("{time}", &time);
    }
    if out.contains("{datetime}") {
        out = out.replace("{datetime}", &format!("{date} {time}"));
    }
    if out.contains("{clipboard}") {
        out = out.replace("{clipboard}", &clipboard_text());
    }
    out
}

fn local_date_time() -> (String, String) {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st = unsafe { GetLocalTime() };
    (
        format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay),
        format!("{:02}:{:02}:{:02}", st.wHour, st.wMinute, st.wSecond),
    )
}

fn clipboard_text() -> String {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        if OpenClipboard(None).is_err() {
            return String::new();
        }
        let text = if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
            match GetClipboardData(CF_UNICODETEXT) {
                Ok(handle) if !handle.is_invalid() => {
                    let ptr = GlobalLock(HGLOBAL(handle.0));
                    let s = if ptr.is_null() {
                        String::new()
                    } else {
                        let wide = ptr as *const u16;
                        let mut len = 0usize;
                        while *wide.add(len) != 0 {
                            len += 1;
                        }
                        let s = String::from_utf16_lossy(std::slice::from_raw_parts(wide, len));
                        let _ = GlobalUnlock(HGLOBAL(handle.0));
                        s
                    };
                    let _ = HANDLE(handle.0);
                    s
                }
                _ => String::new(),
            }
        } else {
            String::new()
        };
        let _ = CloseClipboard();
        text
    }
}

/// Type text into whatever window has focus, using unicode key events so the
/// keyboard layout and the IME do not matter.
fn type_text(text: &str) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE, VIRTUAL_KEY,
    };
    const VK_RETURN: u16 = 0x0D;
    const VK_TAB: u16 = 0x09;

    fn send(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }

    for ch in text.chars() {
        match ch {
            '\n' => {
                send(VK_RETURN, 0, KEYBD_EVENT_FLAGS(0));
                send(VK_RETURN, 0, KEYEVENTF_KEYUP);
            }
            '\t' => {
                send(VK_TAB, 0, KEYBD_EVENT_FLAGS(0));
                send(VK_TAB, 0, KEYEVENTF_KEYUP);
            }
            c => {
                // Characters outside the BMP need one event per UTF-16 unit.
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    send(0, *unit, KEYEVENTF_UNICODE);
                    send(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn open_overlay(ov: &OverlayWindow, st: &mut AppState) {
    let (view_is_wheel, wheel_idx) = match &st.config.route {
        Route::List => (false, st.current_wheel),
        Route::Wheel(i) => (true, (*i).min(st.config.wheels.len().saturating_sub(1))),
    };
    // Opened by the hotkey: the keyboard belongs to the overlay until the key is
    // released, which is what makes releasing without moving a no-op.
    open_overlay_view(ov, st, view_is_wheel, wheel_idx, true);
}

/// Opens the overlay. "by_hotkey" decides whether the keyboard is taken away
/// from the foreground application while it is up; a preview never is.
fn open_overlay_view(
    ov: &OverlayWindow,
    st: &mut AppState,
    view_is_wheel: bool,
    wheel_idx: usize,
    by_hotkey: bool,
) {
    hotkey::set_overlay_engaged(by_hotkey);
    st.current_wheel = wheel_idx.min(st.config.wheels.len().saturating_sub(1));
    st.view_is_wheel = view_is_wheel;
    st.open = true;
    st.hovered = -1;

    rebuild_wheel_model(ov, st);
    rebuild_list_model(ov, st);

    ov.set_show_wheel(view_is_wheel);
    ov.set_hovered(-1);
    ov.set_wheel_index((st.current_wheel + 1) as i32);
    ov.set_wheel_total(st.config.wheels.len() as i32);
    ov.set_reveal(false);
    ov.set_wheel_swap(true);

    let (w, h) = if view_is_wheel {
        (WHEEL_W, WHEEL_H)
    } else {
        list_window_size(st)
    };
    let (center_x, center_y) = if view_is_wheel {
        (DISC_CX, DISC_CY)
    } else {
        (w / 2.0, h / 2.0)
    };

    let cursor = get_cursor_pos();
    let scale = ov.window().scale_factor().max(0.1);
    let w_phys = (w * scale).round() as i32;
    let h_phys = (h * scale).round() as i32;
    let px = cursor.0 - (center_x * scale).round() as i32;
    let py = cursor.1 - (center_y * scale).round() as i32;
    let (px, py) = clamp_to_work_area(px, py, w_phys, h_phys, cursor.0, cursor.1);

    st.overlay_origin = (px, py);
    st.overlay_scale = scale;
    st.open_cursor = cursor;

    ov.window()
        .set_size(PhysicalSize::new(w_phys.max(1) as u32, h_phys.max(1) as u32));
    ov.window().set_position(PhysicalPosition::new(px, py));

    if view_is_wheel {
        apply_backdrop(ov, &st.config, px, py, w_phys, h_phys, scale);
    } else {
        ov.set_disc_image_set(false);
    }

    ntrace!(
        "open view={} wheel={} origin=({px},{py}) size={w_phys}x{h_phys}",
        if view_is_wheel { "wheel" } else { "list" },
        st.current_wheel + 1
    );

    // Whoever is in front must stay in front: the overlay is a popup over the
    // user's application, and stealing activation would send the keystrokes they
    // release into the wrong place.
    let previous_foreground = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()
    };
    let _ = ov.show();
    make_overlay_non_activating();
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
        // The very first ShowWindow activates the overlay before the extended
        // style exists, so hand the foreground straight back.
        if !previous_foreground.is_invalid() && GetForegroundWindow() != previous_foreground {
            let _ = SetForegroundWindow(previous_foreground);
        }
    }

    // One frame has to be rendered in the "closed" state for the entrance
    // transition to have a value to animate from. Without this the window is
    // first painted fully revealed and every entrance animation is skipped.
    let gen = OVERLAY_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let ov_weak = ov.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(16), move || {
        if OVERLAY_GEN.load(Ordering::SeqCst) != gen {
            return;
        }
        if let Some(ov) = ov_weak.upgrade() {
            ov.set_reveal(true);
        }
    });
}

fn close_overlay(ov: &OverlayWindow, st: &mut AppState) {
    st.open = false;
    st.hovered = -1;
    hotkey::set_overlay_engaged(false);
    ov.set_reveal(false);
    ov.set_hovered(-1);

    // Let the exit transition play before the window actually disappears.
    let linger = anim_ms(&st.config, 150, st.config.animation.fade_mult) + 20;
    ntrace!("close overlay, hiding in {linger}ms");
    let gen = OVERLAY_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let ov_weak = ov.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(linger), move || {
        if OVERLAY_GEN.load(Ordering::SeqCst) != gen {
            return;
        }
        if let Some(ov) = ov_weak.upgrade() {
            let _ = ov.hide();
        }
    });
}

/// True when the id belongs to the fixed "open the list view" entry.
fn is_list_entry(st: &AppState, id: i32) -> bool {
    st.config
        .wheels
        .iter()
        .flat_map(|w| w.items.iter())
        .any(|it| it.id as i32 == id && it.is_list_entry)
}

fn activate_and_close(ov: &OverlayWindow, state: &Arc<Mutex<AppState>>, id: i32) {
    // The fixed "list view" entry runs nothing: it switches the overlay to the
    // flat list instead. The list then stays open on its own (nothing is being
    // held down any more), so a card can simply be clicked - or, if the hotkey
    // is still held, released over.
    {
        let st = state.lock().unwrap();
        if is_list_entry(&st, id) {
            drop(st);
            switch_to_list_view(ov, state);
            return;
        }
    }
    let mut st = state.lock().unwrap();
    // Close first: that returns the keyboard, which the text action needs (a
    // hotkey-triggered overlay keeps the keyboard swallowed until it closes).
    close_overlay(ov, &mut st);
    activate_id(&st.config, id);
}

/// Wheel -> list: let the wheel fade out first, then swap the content and the
/// window size while nothing is drawn, and finally fade the cards in. Doing the
/// resize and the content swap in one frame is what made the switch flash.
fn switch_to_list_view(ov: &OverlayWindow, state: &Arc<Mutex<AppState>>) {
    let fade = {
        let st = state.lock().unwrap();
        ntrace!("wheel -> list view (wheel {})", st.current_wheel + 1);
        anim_ms(&st.config, 130, st.config.animation.fade_mult).max(16)
    };
    ov.set_reveal(false);
    let gen = OVERLAY_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let weak = ov.as_weak();
    let st2 = state.clone();
    slint::Timer::single_shot(std::time::Duration::from_millis(fade), move || {
        if OVERLAY_GEN.load(Ordering::SeqCst) != gen {
            return;
        }
        let Some(ov) = weak.upgrade() else { return };
        let mut st = st2.lock().unwrap();
        if !st.open {
            return;
        }
        let wheel = st.current_wheel;
        open_overlay_view(&ov, &mut st, false, wheel, false);
    });
}

fn list_window_size(st: &AppState) -> (f32, f32) {
    let cols = list_columns(&st.config) as f32;
    let count = st.list_ids.len().max(1) as f32;
    let rows = (count / cols).ceil().max(1.0);
    let w = 2.0 * LIST_MARGIN + cols * LIST_CARD_W + (cols - 1.0) * LIST_GAP;
    let h = 2.0 * LIST_MARGIN + rows * LIST_CARD_H + (rows - 1.0) * LIST_GAP;
    (w, h)
}

fn item_at_cursor(st: &AppState) -> Option<i32> {
    let (ox, oy) = st.overlay_origin;
    let scale = st.overlay_scale.max(0.1);
    let cursor = get_cursor_pos();
    let rel_x = (cursor.0 - ox) as f32 / scale;
    let rel_y = (cursor.1 - oy) as f32 / scale;

    if st.view_is_wheel {
        let idx = wheel_sector_at(rel_x, rel_y, st);
        if idx < 0 {
            return None;
        }
        return Some(st.config.wheels[st.current_wheel].items[idx as usize].id as i32);
    }

    let idx = grid_index_at(rel_x, rel_y, st);
    if idx < 0 {
        return None;
    }
    st.list_ids.get(idx as usize).copied().map(|id| id as i32)
}

// ================= platform helpers =================

fn get_cursor_pos() -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

fn clamp_to_work_area(px: i32, py: i32, w: i32, h: i32, cx: i32, cy: i32) -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    };
    let pt = POINT { x: cx, y: cy };
    let hmon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY) };
    let mut mi = MONITORINFO::default();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    unsafe {
        let _ = GetMonitorInfoW(hmon, &mut mi);
    }
    let (left, top, right, bottom) = (
        mi.rcWork.left,
        mi.rcWork.top,
        mi.rcWork.right,
        mi.rcWork.bottom,
    );
    let px = px.clamp(left, (right - w).max(left));
    let py = py.clamp(top, (bottom - h).max(top));
    (px, py)
}

/// Keep the overlay out of the taskbar / alt-tab and, more importantly, stop it
/// from stealing activation: the application the user was working in must keep
/// focus, otherwise the keystroke they release into it goes nowhere.
fn make_overlay_non_activating() {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetWindowLongW, SetWindowLongW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
    };
    static STYLED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if STYLED.load(Ordering::SeqCst) == 1 {
        return;
    }
    let Ok(hwnd) = (unsafe { FindWindowW(None, w!("NoKeys Overlay")) }) else {
        return;
    };
    if hwnd.is_invalid() {
        return;
    }
    unsafe {
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
        SetWindowLongW(
            hwnd,
            GWL_EXSTYLE,
            ex | WS_EX_NOACTIVATE.0 as i32 | WS_EX_TOOLWINDOW.0 as i32,
        );
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    STYLED.store(1, Ordering::SeqCst);
    ntrace!("overlay window marked non-activating");
}

fn hotkey_label(h: &config::Hotkey) -> String {
    let mut parts: Vec<String> = Vec::new();
    if h.ctrl {
        parts.push("Ctrl".into());
    }
    if h.alt {
        parts.push("Alt".into());
    }
    if h.shift {
        parts.push("Shift".into());
    }
    if h.win {
        parts.push("Win".into());
    }
    if h.mouse != 0 {
        let name = match h.mouse as u16 {
            0x01 => "鼠标左键",
            0x02 => "鼠标右键",
            0x04 => "鼠标中键",
            0x05 => "鼠标侧键1",
            0x06 => "鼠标侧键2",
            _ => "鼠标键",
        };
        parts.push(name.into());
    } else {
        parts.push(vk_name(h.key));
    }
    if parts.is_empty() {
        "未绑定".into()
    } else {
        parts.join(" + ")
    }
}

fn vk_name(vk: u32) -> String {
    match vk as u16 {
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0d => "Enter".into(),
        0x13 => "Pause".into(),
        0x14 => "CapsLock".into(),
        0x1b => "Esc".into(),
        0x20 => "Space".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "←".into(),
        0x26 => "↑".into(),
        0x27 => "→".into(),
        0x28 => "↓".into(),
        0x2d => "Insert".into(),
        0x2e => "Delete".into(),
        0x30..=0x39 => ((vk as u8 - 0x30) as char).to_string(),
        0x41..=0x5a => ((vk as u8) as char).to_string(),
        0x5b => "Win".into(),
        0x60..=0x69 => format!("小键盘{}", vk as u8 - 0x60),
        0x70..=0x87 => format!("F{}", vk as u16 - 0x6f),
        0xba => ";".into(),
        0xbb => "=".into(),
        0xbc => ",".into(),
        0xbd => "-".into(),
        0xbe => ".".into(),
        0xbf => "/".into(),
        0xc0 => "~".into(),
        0xdb => "[".into(),
        0xdc => "\\".into(),
        0xdd => "]".into(),
        0xde => "'".into(),
        _ => format!("键{vk}"),
    }
}

/// Capture + blur the desktop behind the disc and hand it to the UI as a texture.
fn apply_backdrop(
    ov: &OverlayWindow,
    config: &Config,
    px: i32,
    py: i32,
    w_phys: i32,
    h_phys: i32,
    scale: f32,
) {
    // Strength 0 means "no glass": capturing would only produce a sharp copy of
    // the desktop inside the disc, which looks like a glitch instead of a panel.
    if !config.blur.enabled || config.blur.strength <= 0.01 {
        ov.set_disc_image_set(false);
        return;
    }
    // Disc box in device coords inside the window.
    let dx = (DISC_X * scale).round() as i32;
    let dy = (DISC_Y * scale).round() as i32;
    let dw = (DISC_D * scale).round() as i32;

    let captured = backdrop::capture(px + dx, py + dy, dw.min(w_phys), dw.min(h_phys));
    let Some(mut shot) = captured else {
        ov.set_disc_image_set(false);
        ntrace!("backdrop capture failed");
        return;
    };

    // strength 0..1 -> blur radius 0..~10 device px (3 box passes ~ gaussian)
    let radius = (config.blur.strength.clamp(0.0, 1.0) * 10.0 * scale).round() as u32;
    backdrop::blur(&mut shot, radius);

    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        &shot.pixels,
        shot.width,
        shot.height,
    );
    ov.set_disc_image(slint::Image::from_rgba8(buffer));
    ov.set_disc_image_set(true);
}

// ================= single instance =================

/// Keep only one resident instance. Returns true when this process is the owner.
fn claim_single_instance() -> bool {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let name = HSTRING::from(r"Local\NoKeys.SingleInstance");
    let handle = unsafe { CreateMutexW(None, true, &name) };
    if handle.is_err() {
        return true;
    }
    // Keep the handle alive for the whole process lifetime.
    std::mem::forget(handle);
    let last = unsafe { GetLastError() };
    last != ERROR_ALREADY_EXISTS
}

/// Bring the already-running instance's settings window to the front.
fn focus_existing_instance() {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };
    let title = HSTRING::from("No Keys - 设置");
    unsafe {
        if let Ok(hwnd) = FindWindowW(None, &title) {
            if !hwnd.is_invalid() {
                let _ = ShowWindow(hwnd, if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW });
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}