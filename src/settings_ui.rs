// Settings dialog wiring: keeps the Slint properties in sync with the config.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::config::{self, Config};
use crate::ntrace;
use crate::hotkey;
use crate::{
    apply_animation, apply_theme, load_icon, hotkey_label, AppState, EditItem,
    OverlayWindow, SettingsWindow,
};

// ================= settings wiring =================

/// Push the whole config into the settings window's properties/models.
pub fn sync_settings(settings: &SettingsWindow, state: &Arc<Mutex<AppState>>) {
    let st = state.lock().unwrap();
    let cfg = &st.config;

    settings.set_hotkey_label(hotkey_label(&cfg.hotkey).into());
    settings.set_wheel_count(cfg.wheels.len() as i32);
    settings.set_route_index(cfg.route_index() as i32);
    settings.set_blur_enabled(cfg.blur.enabled);
    settings.set_blur_strength(cfg.blur.strength);

    settings.set_selected_wheel(st.selected_wheel as i32);
    settings.set_edit_items(ModelRc::new(VecModel::from(build_edit_items(cfg))));

    settings.set_list_bg_enabled(cfg.list_background.enabled);
    settings.set_list_bg_image(cfg.list_background.image.as_str().into());
    settings.set_list_bg_image_set(!cfg.list_background.image.is_empty());
    settings.set_list_bg_opacity(cfg.list_background.opacity);
    settings.set_card_opacity(cfg.card_opacity);
    settings.set_list_columns(cfg.list_columns.clamp(2, 8) as i32);

    let a = &cfg.animation;
    settings.set_anim_global(a.global_speed);
    settings.set_anim_fade(a.fade_mult);
    settings.set_anim_slide(a.slide_mult);
    settings.set_anim_hover_fade(a.hover_fade_mult);
    settings.set_anim_hover_scale(a.hover_scale_mult);
    settings.set_anim_stagger(a.stagger_mult);

    settings.set_data_dir(config::config_dir().display().to_string().into());
    settings.set_capturing(st.capturing);
    // -1 = no item is binding a shortcut
    settings.set_capture_wheel(st.capture_item.map(|(w, _)| w as i32).unwrap_or(-1));
    settings.set_capture_slot(st.capture_item.map(|(_, s)| s as i32).unwrap_or(-1));
}

fn build_edit_items(cfg: &Config) -> Vec<EditItem> {
    let mut out = Vec::new();
    for (wi, wheel) in cfg.wheels.iter().enumerate() {
        for (si, item) in wheel.items.iter().enumerate() {
            let icon = load_icon(&item.icon);
            let has_icon = !item.icon.is_empty() && icon.size().width > 0;
            out.push(EditItem {
                wheel: wi as i32,
                slot: si as i32,
                name: item.name.as_str().into(),
                icon,
                has_icon,
                is_list_entry: item.is_list_entry,
                nokeys_id: item.id as i32,
                action_kind: item.action.kind_index() as i32,
                action_param: item.action.param().into(),
                action_args: item.action.args().into(),
                vk_name: item.vk.map(config::vk_name).unwrap_or_default().into(),
            });
        }
    }
    out
}

/// Re-apply theme / animation / style colours to the overlay after config changes.
pub fn refresh_overlay_style(overlay: &OverlayWindow, state: &Arc<Mutex<AppState>>) {
    let st = state.lock().unwrap();
    let cfg = &st.config;
    apply_theme(overlay);
    apply_animation(overlay, cfg);
    // The configured glass colour now really tints the frosted snapshot; it used
    // to be a dead setting. Kept faint so the disc still reads as glass.
    overlay.set_blur_tint(parse_color_alpha(GLASS_COLOR, 0.18));
    overlay.set_blur_tint_2(parse_color_alpha(GLASS_COLOR, 0.34));
    overlay.set_card_bg(parse_color_alpha(CARD_COLOR, cfg.card_opacity));
    overlay.set_wheel_total(cfg.wheels.len() as i32);
    overlay.set_list_bg_enabled(cfg.list_background.enabled);
    overlay.set_list_bg_color(parse_color_alpha(LIST_BG_COLOR, 1.0));
    overlay.set_list_bg_opacity(cfg.list_background.opacity);
    overlay.set_list_bg_image_set(!cfg.list_background.image.is_empty());
    if !cfg.list_background.image.is_empty() {
        overlay.set_list_bg_image(load_icon(&cfg.list_background.image));
    }
}

/// Fixed colours: the overlay look is no longer configurable.
const CARD_COLOR: &str = "#22242e";
const LIST_BG_COLOR: &str = "#1e1e2e";
const GLASS_COLOR: &str = "#1f2430";

fn parse_color_alpha(hex: &str, alpha: f32) -> slint::Color {
    let h = hex.trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(0xffffff);
    let (r, g, b) = (
        ((v >> 16) & 0xff) as u8,
        ((v >> 8) & 0xff) as u8,
        (v & 0xff) as u8,
    );
    slint::Color::from_argb_u8((alpha.clamp(0.0, 1.0) * 255.0) as u8, r, g, b)
}

/// Save config and refresh both windows.
fn commit(settings: &SettingsWindow, overlay: &OverlayWindow, state: &Arc<Mutex<AppState>>) {
    config::save(&state.lock().unwrap().config).ok();
    sync_settings(settings, state);
    refresh_overlay_style(overlay, state);
}

/// Copy a user-picked icon next to the config, returning its new path.
fn copy_icon(path: &std::path::Path) -> std::io::Result<String> {
    let dir = config::icons_dir();
    std::fs::create_dir_all(&dir)?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "icon.png".into());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let target = dir.join(format!("{stamp}_{name}"));
    std::fs::copy(path, &target)?;
    Ok(target.display().to_string())
}

pub fn wire_settings(
    settings: &SettingsWindow,
    overlay: &OverlayWindow,
    state: &Arc<Mutex<AppState>>,
) {
    // ---------- page transition ----------
    {
        let sw = settings.as_weak();
        settings.on_page_changed(move |_page: i32| {
            // Fade the content back in: drop it to transparent now, raise it on
            // the next frame. A page that is built while already opaque never
            // animates, which is why switching pages used to feel like a hard cut.
            if let Some(se) = sw.upgrade() {
                se.set_content_alpha(0.0);
            }
            let later = sw.clone();
            slint::Timer::single_shot(Duration::from_millis(16), move || {
                if let Some(se) = later.upgrade() {
                    se.set_content_alpha(1.0);
                }
            });
        });
    }

    // ---------- general ----------
    {
        let st = state.clone();
        let sw = settings.as_weak();
        // Always *start* capturing (never toggle): a duplicated click event must not
        // cancel the capture it just started.
        settings.on_request_capture(move || {
            {
                let mut s_ = st.lock().unwrap();
                s_.capturing = true;
                // Retarget: a pending item bind must not swallow this chord.
                s_.capture_item = None;
            }
            hotkey::set_capturing(true);
            ntrace!("request_capture -> capturing=true");
            if let Some(se) = sw.upgrade() {
                sync_settings(&se, &st);
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        settings.on_cancel_capture(move || {
            {
                let mut s_ = st.lock().unwrap();
                s_.capturing = false;
                s_.capture_item = None;
            }
            hotkey::set_capturing(false);
            ntrace!("cancel_capture");
            if let Some(se) = sw.upgrade() {
                sync_settings(&se, &st);
            }
        });
    }

    {
        // Bind the next captured chord to one item instead of the global hotkey.
        let st = state.clone();
        let sw = settings.as_weak();
        settings.on_bind_item_shortcut(move |wheel: i32, slot: i32| {
            if let Some(se) = sw.upgrade() {
                let (wi, si) = (wheel.max(0) as usize, slot.max(0) as usize);
                {
                    let mut s_ = st.lock().unwrap();
                    s_.capturing = true;
                    s_.capture_item = Some((wi, si));
                }
                hotkey::set_capturing(true);
                se.set_capture_hint("".into());
                ntrace!("bind item shortcut {wi}/{si}");
                sync_settings(&se, &st);
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_set_route(move |idx: i32| {
            if let Some(se) = sw.upgrade() {
                st.lock().unwrap().config.set_route_index(idx.max(0) as usize);
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_set_blur_enabled(move |v: bool| {
            if let Some(se) = sw.upgrade() {
                st.lock().unwrap().config.blur.enabled = v;
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_set_blur_strength(move |v: f32| {
            if let Some(se) = sw.upgrade() {
                st.lock().unwrap().config.blur.strength = v.clamp(0.0, 1.0);
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    // ---------- wheels ----------
    {
        let st = state.clone();
        let sw = settings.as_weak();
        settings.on_select_wheel(move |idx: i32| {
            st.lock().unwrap().selected_wheel = idx.max(0) as usize;
            if let Some(se) = sw.upgrade() {
                sync_settings(&se, &st);
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_add_wheel(move || {
            if let Some(se) = sw.upgrade() {
                {
                    let mut s = st.lock().unwrap();
                    if s.config.wheels.len() < 64 {
                        s.config.wheels.push(config::Wheel { items: Vec::new() });
                        s.selected_wheel = s.config.wheels.len() - 1;
                    }
                }
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_remove_wheel(move |idx: i32| {
            if let Some(se) = sw.upgrade() {
                {
                    let mut s = st.lock().unwrap();
                    let i = idx.max(0) as usize;
                    if s.config.wheels.len() > 1 && i < s.config.wheels.len() {
                        s.config.wheels.remove(i);
                        s.selected_wheel = s.selected_wheel.min(s.config.wheels.len() - 1);
                        if s.current_wheel >= s.config.wheels.len() {
                            s.current_wheel = 0;
                        }
                        if let config::Route::Wheel(r) = s.config.route {
                            if r >= s.config.wheels.len() {
                                s.config.route = config::Route::Wheel(0);
                            }
                        }
                        s.config.ensure_list_entry();
                    }
                }
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_add_item(move |wheel: i32| {
            if let Some(se) = sw.upgrade() {
                {
                    let mut s = st.lock().unwrap();
                    let wi = wheel.max(0) as usize;
                    if wi < s.config.wheels.len() && s.config.wheels[wi].items.len() < 8 {
                        let id = s.config.free_id();
                        s.config.wheels[wi].items.push(config::Item {
                            id,
                            name: format!("功能 {id}"),
                            icon: String::new(),
                            is_list_entry: false,
                            vk: None,
                            action: config::Action::default(),
                        });
                        // New items default to the reserved-key action, which
                        // needs a key from the pool right away.
                        let si = s.config.wheels[wi].items.len() - 1;
                        s.config.sync_vk(wi, si);
                    }
                }
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        settings.on_set_item_name(move |wheel: i32, slot: i32, name: SharedString| {
            let mut s = st.lock().unwrap();
            let (wi, si) = (wheel.max(0) as usize, slot.max(0) as usize);
            if let Some(w) = s.config.wheels.get_mut(wi) {
                if let Some(it) = w.items.get_mut(si) {
                    it.name = name.to_string();
                }
            }
            config::save(&s.config).ok();
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_pick_item_icon(move |wheel: i32, slot: i32| {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("图片", &["png", "jpg", "jpeg", "bmp", "gif", "webp", "ico", "svg"])
                .pick_file()
            else {
                return;
            };
            let Some(se) = sw.upgrade() else { return };
            if let Ok(copied) = copy_icon(&path) {
                let mut s = st.lock().unwrap();
                let (wi, si) = (wheel.max(0) as usize, slot.max(0) as usize);
                if let Some(w) = s.config.wheels.get_mut(wi) {
                    if let Some(it) = w.items.get_mut(si) {
                        it.icon = copied;
                    }
                }
                drop(s);
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    // ---------- item actions ----------
    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_set_item_action_kind(move |wheel: i32, slot: i32, kind: i32| {
            if let Some(se) = sw.upgrade() {
                {
                    let mut s = st.lock().unwrap();
                    let (wi, si) = (wheel.max(0) as usize, slot.max(0) as usize);
                    if let Some(it) = s
                        .config
                        .wheels
                        .get_mut(wi)
                        .and_then(|w| w.items.get_mut(si))
                    {
                        it.action = config::Action::with_kind(kind.max(0) as u32, &it.action);
                    }
                    s.config.sync_vk(wi, si);
                }
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        settings.on_set_item_action_param(move |wheel: i32, slot: i32, text: SharedString| {
            let mut s = st.lock().unwrap();
            let (wi, si) = (wheel.max(0) as usize, slot.max(0) as usize);
            if let Some(it) = s
                .config
                .wheels
                .get_mut(wi)
                .and_then(|w| w.items.get_mut(si))
            {
                it.action.set_param(text.to_string());
            }
            s.config.sync_vk(wi, si);
            config::save(&s.config).ok();
        });
    }

    {
        let st = state.clone();
        settings.on_set_item_action_args(move |wheel: i32, slot: i32, text: SharedString| {
            let mut s = st.lock().unwrap();
            let (wi, si) = (wheel.max(0) as usize, slot.max(0) as usize);
            if let Some(it) = s
                .config
                .wheels
                .get_mut(wi)
                .and_then(|w| w.items.get_mut(si))
            {
                it.action.set_args(text.to_string());
            }
            config::save(&s.config).ok();
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_pick_item_program(move |wheel: i32, slot: i32| {
            let picked = rfd::FileDialog::new()
                .add_filter("程序", &["exe", "bat", "cmd", "lnk", "com"])
                .pick_file();
            let Some(path) = picked else { return };
            let Some(se) = sw.upgrade() else { return };
            {
                let mut s = st.lock().unwrap();
                let (wi, si) = (wheel.max(0) as usize, slot.max(0) as usize);
                if let Some(it) = s
                    .config
                    .wheels
                    .get_mut(wi)
                    .and_then(|w| w.items.get_mut(si))
                {
                    it.action = config::Action::with_kind(1, &it.action);
                    it.action.set_param(path.display().to_string());
                }
            }
            if let Some(ov) = ow.upgrade() {
                commit(&se, &ov, &st);
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_set_list_columns(move |cols: i32| {
            if let Some(se) = sw.upgrade() {
                st.lock().unwrap().config.list_columns = cols.clamp(2, 8) as u32;
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_clear_item(move |wheel: i32, slot: i32| {
            if let Some(se) = sw.upgrade() {
                {
                    let mut s = st.lock().unwrap();
                    let (wi, si) = (wheel.max(0) as usize, slot.max(0) as usize);
                    if let Some(w) = s.config.wheels.get_mut(wi) {
                        if si < w.items.len() && !w.items[si].is_list_entry {
                            w.items.remove(si);
                        }
                    }
                }
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    // ---------- list view ----------
    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_set_list_bg_enabled(move |v: bool| {
            if let Some(se) = sw.upgrade() {
                st.lock().unwrap().config.list_background.enabled = v;
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_pick_list_bg_image(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("图片", &["png", "jpg", "jpeg", "bmp", "gif", "webp"])
                .pick_file()
            else {
                return;
            };
            let Some(se) = sw.upgrade() else { return };
            if let Ok(copied) = copy_icon(&path) {
                st.lock().unwrap().config.list_background.image = copied;
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_clear_list_bg_image(move || {
            if let Some(se) = sw.upgrade() {
                st.lock().unwrap().config.list_background.image = String::new();
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_set_list_bg_opacity(move |v: f32| {
            if let Some(se) = sw.upgrade() {
                st.lock().unwrap().config.list_background.opacity = v.clamp(0.0, 1.0);
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_set_card_opacity(move |v: f32| {
            if let Some(se) = sw.upgrade() {
                st.lock().unwrap().config.card_opacity = v.clamp(0.0, 1.0);
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }

    // ---------- animation ----------
    macro_rules! anim_wiring {
        ($cb:ident, $field:ident) => {{
            let st = state.clone();
            let sw = settings.as_weak();
            let ow = overlay.as_weak();
            settings.$cb(move |v: f32| {
                if let Some(se) = sw.upgrade() {
                    st.lock().unwrap().config.animation.$field = v.clamp(0.05, 5.0);
                    if let Some(ov) = ow.upgrade() {
                        commit(&se, &ov, &st);
                    }
                }
            });
        }};
    }
    anim_wiring!(on_set_anim_global, global_speed);
    anim_wiring!(on_set_anim_fade, fade_mult);
    anim_wiring!(on_set_anim_slide, slide_mult);
    anim_wiring!(on_set_anim_hover_fade, hover_fade_mult);
    anim_wiring!(on_set_anim_hover_scale, hover_scale_mult);
    anim_wiring!(on_set_anim_stagger, stagger_mult);

    // ---------- data ----------
    settings.on_open_data_dir(move || {
        let dir = config::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        std::process::Command::new("explorer")
            .arg(dir.display().to_string())
            .spawn()
            .ok();
    });

    {
        let st = state.clone();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        settings.on_clear_all(move || {
            let dir = config::config_dir();
            let _ = std::fs::remove_dir_all(&dir);
            {
                let mut s = st.lock().unwrap();
                s.config = config::Config::fresh();
                s.selected_wheel = 0;
                s.current_wheel = 0;
            }
            config::save(&st.lock().unwrap().config).ok();
            if let Some(se) = sw.upgrade() {
                if let Some(ov) = ow.upgrade() {
                    commit(&se, &ov, &st);
                }
            }
        });
    }
}
