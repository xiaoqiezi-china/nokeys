mod backdrop;
mod config;
mod settings_ui;
#[macro_use]
mod trace;
mod hotkey;

slint::include_modules!();

use std::sync::{mpsc, Arc, Mutex};

use config::{Config, Item, Route, Theme};
use settings_ui::{refresh_overlay_style, sync_settings, wire_settings};
use hotkey::HotkeyEvent;
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
const CONTENT_R: f32 = 132.0;
const X_RADIUS: f32 = 26.0;

const LIST_MARGIN: f32 = 36.0;
const CARD_W: f32 = 320.0;
const CARD_H: f32 = 46.0;
const CARD_GAP: f32 = 10.0;

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
}

fn main() -> Result<(), slint::PlatformError> {
    // The software renderer keeps resident memory ~3.5x lower than the OpenGL
    // (femtovg) path, which matters for this always-on utility. It supports
    // translucent windows, gradients and our captured backdrop blur; the only
    // thing it lacks is `transform-scale`, which we express as size animation.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        std::env::set_var("SLINT_BACKEND", "winit-software");
    }

    if std::env::var("NOKEYS_SELFTEST").is_ok() {
        #[cfg(debug_assertions)]
        {
            let cfg = config::load();
            for (step, ok) in hotkey::selftest(cfg.hotkey.clone()) {
                println!("{} {}: {}", if ok { "PASS" } else { "FAIL" }, step, if ok { "" } else { "<-- broken" });
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

    let overlay = OverlayWindow::new()?;
    let settings = SettingsWindow::new()?;

    let state = Arc::new(Mutex::new(AppState {
        current_wheel: 0,
        hovered: -1,
        open: false,
        view_is_wheel: true,
        list_ids: Vec::new(),
        capturing: false,
        selected_wheel: 0,
        config,
    }));

    // initial theme + animation + models
    {
        let mut st = state.lock().unwrap();
        apply_theme(&overlay, is_dark(&st.config));
        apply_animation(&overlay, &st.config);
        overlay.set_show_wheel(true);
        overlay.set_wheel_index(1);
        overlay.set_reveal(false);
        rebuild_wheel_model(&overlay, &st);
        rebuild_list_model(&overlay, &mut st);
    }

    // ---- overlay callbacks ----
    let state_hover = state.clone();
    let overlay_weak_hover = overlay.as_weak();
    overlay.on_hover_at(move |x: f32, y: f32| {
        if let Some(ov) = overlay_weak_hover.upgrade() {
            let st = state_hover.lock().unwrap();
            let idx = wheel_sector_at(x, y, &st);
            drop(st);
            ov.set_hovered(idx);
        }
    });

    let state_click = state.clone();
    let overlay_weak_click = overlay.as_weak();
    overlay.on_click_at(move |x: f32, y: f32| {
        if let Some(ov) = overlay_weak_click.upgrade() {
            let mut st = state_click.lock().unwrap();
            let idx = wheel_sector_at(x, y, &st);
            if idx >= 0 {
                let id = st.config.wheels[st.current_wheel].items[idx as usize].id as i32;
                activate_and_close(&ov, &mut st, id);
            }
        }
    });

    let state_activate = state.clone();
    let overlay_weak_activate = overlay.as_weak();
    overlay.on_activate(move |id: i32| {
        if let Some(ov) = overlay_weak_activate.upgrade() {
            let mut st = state_activate.lock().unwrap();
            activate_and_close(&ov, &mut st, id);
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
            open_overlay_view(&ov, &mut st, true, idx);
        }
    });

    let state_pl = state.clone();
    let overlay_weak_pl = overlay.as_weak();
    settings.on_preview_list(move || {
        if let Some(ov) = overlay_weak_pl.upgrade() {
            let mut st = state_pl.lock().unwrap();
            let idx = st.current_wheel.min(st.config.wheels.len().saturating_sub(1));
            open_overlay_view(&ov, &mut st, false, idx);
        }
    });

    if let Ok(page) = std::env::var("NOKEYS_PAGE") {
        if let Ok(p) = page.parse::<i32>() {
            settings.set_page(p);
        }
    }

    wire_settings(&settings, &overlay, &state);
    sync_settings(&settings, &state);
    refresh_overlay_style(&overlay, &state);

    // ---- global hotkey (installed on the main thread; Slint's loop pumps messages) ----
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
                ntrace!("dispatch ev={:?} settings={} overlay={}", ev, se.is_some(), ov.is_some());
                if matches!(ev, HotkeyEvent::CaptureCancelled) {
                    if let Some(se) = &se {
                        let mut state = st.lock().unwrap();
                        state.capturing = false;
                        hotkey::set_capturing(false);
                        drop(state);
                        sync_settings(se, &st);
                    }
                    return;
                }
                if let HotkeyEvent::Captured(hk) = ev {
                    if let Some(se) = &se {
                        let mut state = st.lock().unwrap();
                        ntrace!("captured hk={:?} state.capturing={}", hk, state.capturing);
                        if state.capturing {
                            state.capturing = false;
                            state.config.hotkey = hk;
                            hotkey::set_hotkey(hk);
                            config::save(&state.config).ok();
                            drop(state);
                            sync_settings(se, &st);
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

    // Dev harness: NOKEYS_AUTOPEN=wheel|list auto-opens after 2s; NOKEYS_SNAPSHOT=1 dumps a pixel report.
    if std::env::var("NOKEYS_AUTOPEN").is_ok() {
        let state_a = state.clone();
        let ow_a = overlay.as_weak();
        let as_list = std::env::var("NOKEYS_AUTOPEN").unwrap_or_default() == "list";
        let snap = std::env::var("NOKEYS_SNAPSHOT").is_ok();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let ow_open = ow_a.clone();
            let state_open = state_a.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ov) = ow_open.upgrade() {
                    let mut st = state_open.lock().unwrap();
                    let idx = st.current_wheel.min(st.config.wheels.len().saturating_sub(1));
                    open_overlay_view(&ov, &mut st, !as_list, idx);
                    if let Ok(h) = std::env::var("NOKEYS_HOVER") {
                        if let Ok(h) = h.parse::<i32>() {
                            ov.set_hovered(h);
                            st.hovered = h;
                        }
                    }
                }
            });
            if snap {
                std::thread::sleep(std::time::Duration::from_millis(1800));
                let ow_snap = ow_a.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ov) = ow_snap.upgrade() {
                        match ov.window().take_snapshot() {
                            Ok(buf) => debug_pixel_report(&buf),
                            Err(e) => eprintln!("[nokeys] snapshot err: {e}"),
                        }
                    }
                });
            }
        });
    }

    // Diagnostic: report hook liveness from a plain thread (never from the hook).
    if std::env::var("NOKEYS_TRACE").is_ok() {
        std::thread::spawn(|| {
            let mut last = (u32::MAX, u32::MAX, u32::MAX);
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let now = (
                    hotkey::stat_keys(),
                    hotkey::stat_capture_active(),
                    hotkey::stat_capture_hits(),
                );
                if now != last {
                    ntrace!("hook stats keys={} cap_active={} cap_hits={}", now.0, now.1, now.2);
                    last = now;
                }
            }
        });
    }

    // Refresh the low-level hooks periodically: Windows silently removes a hook
    // whose callback timed out, and that is otherwise undetectable.
    {
        let mut tick = 0u32;
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(30));
            tick += 1;
            let _ = slint::invoke_from_event_loop(move || {
                hotkey::reinstall_hooks();
                ntrace!("watchdog tick {}", tick);
            });
        });
    }

    // Follow the Windows light/dark theme while running.
    let theme_timer = slint::Timer::default();
    {
        let ow = overlay.as_weak();
        let sw = settings.as_weak();
        let st = state.clone();
        let mut last_dark = system_uses_dark_theme();
        theme_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(2),
            move || {
                let dark = system_uses_dark_theme();
                if dark == last_dark {
                    return;
                }
                last_dark = dark;
                let follow = matches!(st.lock().unwrap().config.theme, Theme::Auto);
                if !follow {
                    return;
                }
                if let Some(ov) = ow.upgrade() {
                    refresh_overlay_style(&ov, &st);
                }
                if let Some(se) = sw.upgrade() {
                    sync_settings(&se, &st);
                }
            },
        );
    }

    // show settings window and run the event loop
    settings.run()
}

// ================= helpers =================

fn is_dark(config: &Config) -> bool {
    match config.theme {
        Theme::Auto => system_uses_dark_theme(),
        Theme::Light => false,
        Theme::Dark => true,
    }
}

fn system_uses_dark_theme() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) =
        hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
    {
        if let Ok(v) = key.get_value::<u32, _>("AppsUseLightTheme") {
            return v == 0;
        }
    }
    true
}

fn apply_theme(ov: &OverlayWindow, dark: bool) {
    if dark {
        ov.set_disc_tint(Color::from_argb_u8(0xcc, 0x3a, 0x3f, 0x4e));
        ov.set_disc_tint_2(Color::from_argb_u8(0xe0, 0x17, 0x1a, 0x22));
        ov.set_blur_tint(Color::from_argb_u8(0x4d, 0x1f, 0x24, 0x30));
        ov.set_blur_tint_2(Color::from_argb_u8(0x66, 0x12, 0x15, 0x1b));
        ov.set_disc_border(Color::from_argb_u8(0x1f, 0xff, 0xff, 0xff));
        ov.set_sector_stroke(Color::from_argb_u8(0x26, 0xff, 0xff, 0xff));
        ov.set_sector_hover(Color::from_argb_u8(0x2e, 0xff, 0xff, 0xff));
        ov.set_text_color(Color::from_argb_u8(0xff, 0xff, 0xff, 0xff));
        ov.set_card_bg(Color::from_argb_u8(0xd8, 0x22, 0x24, 0x2e));
        ov.set_sub_text_color(Color::from_argb_u8(0xb0, 0xff, 0xff, 0xff));
        ov.set_close_bg(Color::from_argb_u8(0x14, 0xff, 0xff, 0xff));
        ov.set_close_hover_bg(Color::from_argb_u8(0x66, 0xff, 0x5a, 0x5f));
        ov.set_icon_ph_bg(Color::from_argb_u8(0x1a, 0xff, 0xff, 0xff));
        ov.set_icon_ph_line(Color::from_argb_u8(0x66, 0xff, 0xff, 0xff));
        ov.set_chip_bg(Color::from_argb_u8(0x38, 0x00, 0x00, 0x00));
    } else {
        ov.set_disc_tint(Color::from_argb_u8(0xd8, 0xf7, 0xf9, 0xfc));
        ov.set_disc_tint_2(Color::from_argb_u8(0xe8, 0xdd, 0xe1, 0xe8));
        ov.set_blur_tint(Color::from_argb_u8(0x59, 0xf2, 0xf4, 0xf8));
        ov.set_blur_tint_2(Color::from_argb_u8(0x73, 0xe3, 0xe6, 0xec));
        ov.set_disc_border(Color::from_argb_u8(0x24, 0x00, 0x00, 0x00));
        ov.set_sector_stroke(Color::from_argb_u8(0x1c, 0x00, 0x00, 0x00));
        ov.set_sector_hover(Color::from_argb_u8(0x20, 0x00, 0x00, 0x00));
        ov.set_text_color(Color::from_argb_u8(0xff, 0x1a, 0x1c, 0x22));
        ov.set_card_bg(Color::from_argb_u8(0xee, 0xff, 0xff, 0xff));
        ov.set_sub_text_color(Color::from_argb_u8(0xb0, 0x1a, 0x1c, 0x22));
        ov.set_close_bg(Color::from_argb_u8(0x10, 0x00, 0x00, 0x00));
        ov.set_close_hover_bg(Color::from_argb_u8(0x66, 0xe5, 0x48, 0x4d));
        ov.set_icon_ph_bg(Color::from_argb_u8(0x14, 0x00, 0x00, 0x00));
        ov.set_icon_ph_line(Color::from_argb_u8(0x59, 0x00, 0x00, 0x00));
        ov.set_chip_bg(Color::from_argb_u8(0x99, 0xff, 0xff, 0xff));
    }
}

fn apply_animation(ov: &OverlayWindow, config: &Config) {
    let a = &config.animation;
    let dur = |base: i64, mult: f32| -> i64 {
        let s = (a.global_speed * mult).max(0.05);
        ((base as f32 / s).round().max(1.0)) as i64
    };
    ov.set_fade_dur(dur(150, a.fade_mult));
    ov.set_slide_dur(dur(220, a.slide_mult));
    ov.set_hover_dur(dur(130, a.hover_fade_mult.min(a.hover_scale_mult)));
    ov.set_stagger_dur(dur(45, a.stagger_mult));
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
            path: SharedString::from(wedge_path(n, i)),
            cx,
            cy,
        });
    }
    ov.set_wheel_items(ModelRc::new(VecModel::from(model)));
}

fn rebuild_list_model(ov: &OverlayWindow, st: &mut AppState) {
    let mut model: Vec<ListItem> = Vec::new();
    let mut ids: Vec<u32> = Vec::new();
    let cfg = &st.config;
    for wheel in &cfg.wheels {
        for it in &wheel.items {
            if it.is_list_entry {
                continue;
            }
            let cy = LIST_MARGIN + ids.len() as f32 * (CARD_H + CARD_GAP);
            let icon = load_icon(&it.icon);
            let has_icon = !it.icon.is_empty() && icon.size().width > 0;
            model.push(ListItem {
                id: it.id as i32,
                name: SharedString::from(it.name.as_str()),
                icon,
                has_icon,
                cy,
            });
            ids.push(it.id);
        }
    }
    st.list_ids = ids;
    ov.set_list_items(ModelRc::new(VecModel::from(model)));
}

/// Pie sector path in the 360x360 pixel space of the disc box.
fn wedge_path(n: usize, i: usize) -> String {
    if n < 2 {
        return String::new();
    }
    let r = DISC_D / 2.0 * 0.96; // pixel radius (0..360 box)
    let (cx, cy) = (DISC_D / 2.0, DISC_D / 2.0);
    let gap = 2.0;
    let start = -90.0 + i as f64 * (360.0 / n as f64) + gap / 2.0;
    let end = -90.0 + (i + 1) as f64 * (360.0 / n as f64) - gap / 2.0;
    let (x0, y0) = polar(cx as f64, cy as f64, r as f64, start);
    let (x1, y1) = polar(cx as f64, cy as f64, r as f64, end);
    format!(
        "M {cx} {cy} L {x0:.3} {y0:.3} A {r} {r} 0 0 1 {x1:.3} {y1:.3} Z"
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

fn wheel_sector_at(x: f32, y: f32, st: &AppState) -> i32 {
    let n = st.config.wheels[st.current_wheel].items.len();
    if n == 0 {
        return -1;
    }
    let dx = x - DISC_CX;
    let dy = y - DISC_CY;
    let dist = (dx * dx + dy * dy).sqrt();
    if dist > DISC_R || dist < X_RADIUS {
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
                return;
            }
            eprintln!("[nokeys] hotkey down");
            open_overlay(ov, &mut st);
        }
        HotkeyEvent::Up => {
            if !st.open {
                return;
            }
            eprintln!("[nokeys] hotkey up");
            let id = item_at_cursor(ov, &st);
            close_overlay(ov, &mut st);
            if let Some(id) = id {
                println!("[nokeys] activate nokeys_{id}");
            }
        }
        HotkeyEvent::Captured(_) | HotkeyEvent::CaptureCancelled => {
            // handled where the settings window is reachable
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
            rebuild_wheel_model(ov, &st);
        }
    }
}

fn open_overlay(ov: &OverlayWindow, st: &mut AppState) {
    let (view_is_wheel, wheel_idx) = match &st.config.route {
        Route::List => (false, st.current_wheel),
        Route::Wheel(i) => (true, (*i).min(st.config.wheels.len().saturating_sub(1))),
    };
    open_overlay_view(ov, st, view_is_wheel, wheel_idx);
}

fn open_overlay_view(ov: &OverlayWindow, st: &mut AppState, view_is_wheel: bool, wheel_idx: usize) {
    st.current_wheel = wheel_idx;
    st.view_is_wheel = view_is_wheel;
    st.open = true;
    st.hovered = -1;

    rebuild_wheel_model(ov, st);
    rebuild_list_model(ov, st);

    ov.set_show_wheel(view_is_wheel);
    ov.set_hovered(-1);
    ov.set_wheel_index((st.current_wheel + 1) as i32);
    ov.set_reveal(false);

    let (w, h) = if view_is_wheel {
        (WHEEL_W, WHEEL_H)
    } else {
        list_window_size(&st.list_ids)
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

    ov.window()
        .set_size(PhysicalSize::new(w_phys.max(1) as u32, h_phys.max(1) as u32));
    ov.window().set_position(PhysicalPosition::new(px, py));

    if view_is_wheel {
        apply_backdrop(ov, &st.config, px, py, w_phys, h_phys, scale);
    } else {
        ov.set_disc_image_set(false);
    }
    eprintln!(
        "[nokeys] open: view={} wheel={} pos=({},{}) size=({}x{})",
        if view_is_wheel { "wheel" } else { "list" },
        st.current_wheel + 1,
        px,
        py,
        w_phys,
        h_phys
    );
    let _ = ov.show();
    ov.set_reveal(true);
}

fn close_overlay(ov: &OverlayWindow, st: &mut AppState) {
    st.open = false;
    st.hovered = -1;
    ov.set_reveal(false);
    let _ = ov.hide();
}

fn activate_and_close(ov: &OverlayWindow, st: &mut AppState, id: i32) {
    println!("[nokeys] activate nokeys_{id}");
    close_overlay(ov, st);
}

fn list_window_size(list_ids: &[u32]) -> (f32, f32) {
    let w = CARD_W + 2.0 * LIST_MARGIN;
    let h = if list_ids.is_empty() {
        2.0 * LIST_MARGIN + CARD_H
    } else {
        let count = list_ids.len() as f32;
        2.0 * LIST_MARGIN + (count - 1.0) * (CARD_H + CARD_GAP) + CARD_H
    };
    (w, h)
}

fn item_at_cursor(ov: &OverlayWindow, st: &AppState) -> Option<i32> {
    let win = ov.window();
    let pos = win.position();
    let scale = win.scale_factor().max(0.1);
    let cursor = get_cursor_pos();
    let rel_x = (cursor.0 as f32 - pos.x as f32) / scale;
    let rel_y = (cursor.1 as f32 - pos.y as f32) / scale;

    if st.view_is_wheel {
        let idx = wheel_sector_at(rel_x, rel_y, st);
        if idx < 0 {
            return None;
        }
        return Some(st.config.wheels[st.current_wheel].items[idx as usize].id as i32);
    }

    // list view: map y to card index
    if rel_x < LIST_MARGIN || rel_x > LIST_MARGIN + CARD_W {
        return None;
    }
    let step = CARD_H + CARD_GAP;
    let idx_f = (rel_y - LIST_MARGIN) / step;
    if idx_f < 0.0 {
        return None;
    }
    let idx = idx_f as usize;
    let local = rel_y - LIST_MARGIN - idx as f32 * step;
    if local > CARD_H {
        return None;
    }
    st.list_ids.get(idx).copied().map(|id| id as i32)
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

fn clamp_to_work_area(px: i32, py: i32, w: i32, h: i32, cx: i32, cy: i32) -> (i32, i32) {    use windows::Win32::Foundation::POINT;
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
    let (left, top, right, bottom) = (mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom);
    let px = px.clamp(left, (right - w).max(left));
    let py = py.clamp(top, (bottom - h).max(top));
    (px, py)
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
        0x60..=0x69 => format!("小键盘{}", vk as u8 - 0x60),
        0x70..=0x87 => format!("F{}", vk as u16 - 0x6f),
        0xba => ";".into(),
        0xbb => "=".into(),
        0xbc => ",".into(),
        0xbd => "-".into(),
        0xbe => ".".into(),
        0xbf => "/".into(),
        0xc0 => "`".into(),
        0xdb => "[".into(),
        0xdc => "\\".into(),
        0xdd => "]".into(),
        0xde => "'".into(),
        _ => format!("键{}", vk),
    }
}


/// Dev-only: print a coarse pixel map so geometry can be verified without a screen.
fn debug_pixel_report(buf: &slint::SharedPixelBuffer<slint::Rgba8Pixel>) {
    let w = buf.width() as usize;
    let h = buf.height() as usize;
    let px = |x: usize, y: usize| buf.as_slice()[y * w + x];
    eprintln!("[nokeys] snapshot {w}x{h}");

    // coarse ASCII map: '#' opaque-ish, '+' partial, '.' transparent
    let step = 12;
    let mut line = String::new();
    for y in (0..h).step_by(step) {
        line.clear();
        for x in (0..w).step_by(step) {
            let p = px(x, y);
            line.push(if p.a > 150 {
                '#'
            } else if p.a > 20 {
                '+'
            } else {
                '.'
            });
        }
        eprintln!("[nokeys] {line}");
    }

    // sample: disc edge along +x from center
    let (cx, cy) = (216usize, 216usize);
    for r in [30usize, 60, 100, 150, 172, 178, 185] {
        let p = px(cx + r, cy);
        eprintln!("[nokeys] r={r} rgba=({},{},{},{})", p.r, p.g, p.b, p.a);
    }

    // sample chip area of wheel item #2 (logical 105,305 @ scale 1.104)
    for (lx, ly) in [(105usize, 305usize), (146, 240)] {
        let x = (lx as f32 * 1.104) as usize;
        let y = (ly as f32 * 1.104) as usize;
        if x < w && y < h {
            let p = px(x, y);
            eprintln!("[nokeys] logical({lx},{ly}) rgba=({},{},{},{})", p.r, p.g, p.b, p.a);
        }
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
    if !config.blur.enabled {
        ov.set_disc_image_set(false);
        return;
    }
    // Disc box in logical coords inside the window.
    let dx = (DISC_X * scale).round() as i32;
    let dy = (DISC_Y * scale).round() as i32;
    let dw = (DISC_D * scale).round() as i32;

    let captured = backdrop::capture(px + dx, py + dy, dw.min(w_phys), dw.min(h_phys));
    let Some(mut shot) = captured else {
        ov.set_disc_image_set(false);
        eprintln!("[nokeys] backdrop capture failed");
        return;
    };

    // strength 0..1 -> blur radius 0..~10 physical px (3 box passes ≈ gaussian)
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
