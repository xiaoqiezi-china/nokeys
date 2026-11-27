use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Global configuration, persisted as JSON.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Config {
    pub hotkey: Hotkey,
    pub route: Route,
    pub blur: Blur,
    pub list_background: ListBackground,
    /// 0.0 .. 1.0 opacity of the list view cards.
    #[serde(default = "default_card_opacity")]
    pub card_opacity: f32,
    /// Items per row in the list view (cards are 4:3 and wrap after this many).
    #[serde(default = "default_list_columns")]
    pub list_columns: u32,
    pub animation: Animation,
    pub wheels: Vec<Wheel>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Virtual-key code (windows VK_*). 0 means "mouse button" (see `mouse`).
    pub key: u32,
    /// Mouse virtual-key code (e.g. VK_XBUTTON1/2) or 0 when `key` is used.
    pub mouse: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Route {
    /// Open a specific wheel by index (1-based in UI, 0-based here).
    Wheel(usize),
    /// Open the list view directly.
    List,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Blur {
    pub enabled: bool,
    /// 0.0 .. 1.0, strength of the frosted glass (tint opacity / blur radius).
    pub strength: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ListBackground {
    pub enabled: bool,
    /// Path to a background image (empty = none).
    pub image: String,
    /// 0.0 .. 1.0
    pub opacity: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Animation {
    /// Master speed. >1 = faster, <1 = slower.
    pub global_speed: f32,
    pub fade_mult: f32,
    pub slide_mult: f32,
    pub hover_fade_mult: f32,
    pub hover_scale_mult: f32,
    pub stagger_mult: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Wheel {
    /// Ordered items; position in the vector = sector order.
    /// At most 8 visible items per wheel.
    pub items: Vec<Item>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Item {
    /// nokeys_N id: always the lowest free number, so a deleted one is reused.
    pub id: u32,
    /// User-visible function name.
    pub name: String,
    /// Path to icon file (empty = default glyph).
    pub icon: String,
    /// True for the fixed "open list view" entry (only in wheel #1).
    pub is_list_entry: bool,
    /// Reserved virtual key this item emits; empty while it does something else,
    /// so the key returns to the pool for the next item that needs one.
    #[serde(default)]
    pub vk: Option<u16>,
    /// What activating this item does. Older profiles load as Action::Key.
    #[serde(default)]
    pub action: Action,
}

/// What an item does when it is activated.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub enum Action {
    /// Send a key. Empty means the item's reserved key (see VK_POOL);
    /// otherwise the given key or chord is sent instead.
    Key { keys: String },
    /// Start a program, optionally with command line arguments.
    Program { path: String, args: String },
    /// Send a key chord (e.g. "Ctrl+Alt+M", "F8") to the focused window.
    Shortcut { keys: String },
    /// Type text into whatever window has focus; {date} style tokens expand.
    Text { text: String },
    /// Run a console command through cmd.exe, without a console window.
    Command { cmd: String },
    /// Open a URL in the default browser.
    Url { url: String },
}

/// Keys handed out to items that emit a reserved key: F13..F24. No physical
/// keyboard produces them, so they never collide with normal typing, yet other
/// programs can bind them. They are handed out on demand and taken back when an
/// item stops sending one, so the set only covers the items that need it.
pub const VK_POOL: [u16; 12] = [
    0x7C, 0x7D, 0x7E, 0x7F, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
];

pub fn vk_name(vk: u16) -> String {
    match VK_POOL.iter().position(|k| *k == vk) {
        Some(i) => format!("F{}", 13 + i),
        None => format!("0x{vk:02X}"),
    }
}

/// Key that older builds derived from the item id. Preferred when an existing
/// profile is upgraded, so keys already bound in other programs keep working.
fn legacy_vk(id: u32) -> u16 {
    VK_POOL[(id.saturating_sub(1) % VK_POOL.len() as u32) as usize]
}

impl Default for Action {
    fn default() -> Self {
        Action::Key { keys: String::new() }
    }
}

/// Hand-written so that profiles written before the key field existed (where the
/// variant was the plain string "Key") keep loading.
impl<'de> Deserialize<'de> for Action {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Full(Full),
            Legacy(String),
        }
        #[derive(Deserialize)]
        #[allow(clippy::enum_variant_names)]
        enum Full {
            Key {
                #[serde(default)]
                keys: String,
            },
            Shortcut {
                #[serde(default)]
                keys: String,
            },
            Program {
                #[serde(default)]
                path: String,
                #[serde(default)]
                args: String,
            },
            Text {
                #[serde(default)]
                text: String,
            },
            Command {
                #[serde(default)]
                cmd: String,
            },
            Url {
                #[serde(default)]
                url: String,
            },
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Full(Full::Key { keys }) => Action::Key { keys },
            Repr::Full(Full::Shortcut { keys }) => Action::Shortcut { keys },
            Repr::Full(Full::Program { path, args }) => Action::Program { path, args },
            Repr::Full(Full::Text { text }) => Action::Text { text },
            Repr::Full(Full::Command { cmd }) => Action::Command { cmd },
            Repr::Full(Full::Url { url }) => Action::Url { url },
            Repr::Legacy(name) => {
                // Any older form was the reserved-key action; the string is only
                // kept so a future variant can be migrated explicitly.
                debug_assert!(!name.is_empty());
                Action::Key { keys: String::new() }
            }
        })
    }
}

impl Action {
    /// Stable index used between the UI and the config (0..=4).
    pub fn kind_index(&self) -> u32 {
        match self {
            Action::Key { .. } => 0,
            Action::Shortcut { .. } => 1,
            Action::Program { .. } => 2,
            Action::Text { .. } => 3,
            Action::Command { .. } => 4,
            Action::Url { .. } => 5,
        }
    }

    /// Switch the kind, keeping the previous text when it still applies.
    pub fn with_kind(kind: u32, previous: &Action) -> Action {
        let text = previous.param().to_string();
        let args = previous.args().to_string();
        match kind {
            // A shortcut is bound by pressing it, so it never inherits text.
            1 => Action::Shortcut { keys: String::new() },
            2 => Action::Program { path: text, args },
            3 => Action::Text { text },
            4 => Action::Command { cmd: text },
            5 => Action::Url { url: text },
            _ => Action::Key { keys: String::new() },
        }
    }

    /// The free-text parameter of the current kind (empty for Key).
    pub fn param(&self) -> &str {
        match self {
            Action::Key { keys } => keys,
            Action::Shortcut { keys } => keys,
            Action::Program { path, .. } => path,
            Action::Text { text } => text,
            Action::Command { cmd } => cmd,
            Action::Url { url } => url,
        }
    }

    pub fn set_param(&mut self, value: String) {
        match self {
            Action::Key { keys } => *keys = value,
            Action::Shortcut { keys } => *keys = value,
            Action::Program { path, .. } => *path = value,
            Action::Text { text } => *text = value,
            Action::Command { cmd } => *cmd = value,
            Action::Url { url } => *url = value,
        }
    }

    /// Only Program carries a second field.
    pub fn args(&self) -> &str {
        match self {
            Action::Program { args, .. } => args,
            _ => "",
        }
    }

    pub fn set_args(&mut self, value: String) {
        if let Action::Program { args, .. } = self {
            *args = value;
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        // Dev defaults include sample items so the GUI can be exercised.
        // Final defaults will be trimmed to a minimal single wheel.
        Config {
            hotkey: Hotkey {
                ctrl: true,
                alt: true,
                shift: false,
                win: false,
                key: 0x4E, // 'N'
                mouse: 0,
            },
            route: Route::Wheel(0),
            blur: Blur {
                enabled: true,
                strength: 0.5,
            },
            list_background: ListBackground {
                enabled: false,
                image: String::new(),
                opacity: 0.5,
            },
            card_opacity: 0.85,
            list_columns: 4,
            animation: Animation {
                global_speed: 1.0,
                fade_mult: 1.0,
                slide_mult: 1.0,
                hover_fade_mult: 1.0,
                hover_scale_mult: 1.0,
                stagger_mult: 1.0,
            },
            wheels: vec![
                Wheel {
                    items: vec![
                        Item {
                            id: 1,
                            name: "列表视图".into(),
                            icon: String::new(),
                            is_list_entry: true,
                            vk: None,
                            action: Action::default(),
                        },
                        Item {
                            id: 2,
                            name: "示例功能A".into(),
                            icon: String::new(),
                            is_list_entry: false,
                            vk: None,
                            action: Action::default(),
                        },
                        Item {
                            id: 3,
                            name: "示例功能B".into(),
                            icon: String::new(),
                            is_list_entry: false,
                            vk: None,
                            action: Action::default(),
                        },
                        Item {
                            id: 4,
                            name: "示例功能C".into(),
                            icon: String::new(),
                            is_list_entry: false,
                            vk: None,
                            action: Action::default(),
                        },
                    ],
                },
                Wheel {
                    items: vec![
                        Item {
                            id: 5,
                            name: "示例功能D".into(),
                            icon: String::new(),
                            is_list_entry: false,
                            vk: None,
                            action: Action::default(),
                        },
                        Item {
                            id: 6,
                            name: "示例功能E".into(),
                            icon: String::new(),
                            is_list_entry: false,
                            vk: None,
                            action: Action::default(),
                        },
                    ],
                },
            ],
        }
    }
}

fn default_card_opacity() -> f32 {
    0.85
}

fn default_list_columns() -> u32 {
    4
}

pub fn config_dir() -> PathBuf {
    match std::env::var("APPDATA") {
        Ok(appdata) => PathBuf::from(appdata).join("NoKeys"),
        Err(_) => PathBuf::from(".").join("NoKeys"),
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn icons_dir() -> PathBuf {
    config_dir().join("icons")
}

pub fn load() -> Config {
    let path = config_path();
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(text) => {
            // Notepad and friends happily prepend a UTF-8 BOM; serde rejects it
            // and the whole profile would silently fall back to the defaults.
            let text = text.trim_start_matches('﻿');
            serde_json::from_str(text).unwrap_or_else(|e| {
                crate::ntrace!("config parse error ({e}), using defaults");
                Config::default()
            })
        }
        Err(_) => Config::default(),
    };
    // A left/right mouse button would be swallowed before it can reach our own UI,
    // so such a binding can never be used: fall back to the default chord.
    if cfg.hotkey.mouse == 0x01 || cfg.hotkey.mouse == 0x02 {
        cfg.hotkey = Config::default().hotkey;
    }
    cfg
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(icons_dir())?;
    let text = serde_json::to_string_pretty(cfg).expect("serialize config");
    std::fs::write(config_path(), text)
}

impl Config {
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.wheels.iter().flat_map(|w| w.items.iter())
    }

    /// Lowest id no item uses: deleting 7 out of 7..9 hands 7 to the next item.
    pub fn free_id(&self) -> u32 {
        let mut used: Vec<u32> = self.items().map(|i| i.id).collect();
        used.sort_unstable();
        let mut id = 1;
        for u in used {
            if u == id {
                id += 1;
            } else if u > id {
                break;
            }
        }
        id
    }

    /// Lowest key nobody holds. If all 12 are in use a further item has to share
    /// F13, which is far past any reasonable profile.
    pub fn free_vk(&self) -> u16 {
        let used: Vec<u16> = self.items().filter_map(|i| i.vk).collect();
        VK_POOL
            .iter()
            .copied()
            .find(|vk| !used.contains(vk))
            .unwrap_or(VK_POOL[0])
    }

    /// Give the item a key while it emits one, take it back when it does not.
    pub fn sync_vk(&mut self, wi: usize, si: usize) {
        self.sync_vk_impl(wi, si, false);
    }

    /// keep_legacy hands out the key the id used to map to when it is still free,
    /// so keys already bound in other programs survive an upgrade.
    fn sync_vk_impl(&mut self, wi: usize, si: usize, keep_legacy: bool) {
        let Some(item) = self.wheels.get(wi).and_then(|w| w.items.get(si)) else {
            return;
        };
        let wants = !item.is_list_entry
            && matches!(&item.action, Action::Key { keys } if keys.trim().is_empty());
        let next = match (wants, item.vk) {
            (false, _) => None,
            (true, Some(vk)) => Some(vk),
            (true, None) => {
                let preferred = legacy_vk(item.id);
                if keep_legacy && !self.items().any(|i| i.vk == Some(preferred)) {
                    Some(preferred)
                } else {
                    Some(self.free_vk())
                }
            }
        };
        if let Some(it) = self.wheels.get_mut(wi).and_then(|w| w.items.get_mut(si)) {
            it.vk = next;
        }
    }

    /// Run the allocation over the whole profile after loading a file.
    pub fn normalize_vks(&mut self) {
        for wi in 0..self.wheels.len() {
            for si in 0..self.wheels[wi].items.len() {
                self.sync_vk_impl(wi, si, true);
            }
        }
    }

    /// Wheel #0 always carries exactly one fixed "list view" entry at slot 0.
    pub fn ensure_list_entry(&mut self) {
        if self.wheels.is_empty() {
            self.wheels.push(Wheel { items: Vec::new() });
        }
        let has_entry = self.wheels[0].items.iter().any(|i| i.is_list_entry);
        if !has_entry {
            let id = self.free_id();
            self.wheels[0].items.insert(
                0,
                Item {
                    id,
                    name: "列表视图".into(),
                    icon: String::new(),
                    is_list_entry: true,
                    vk: None,
                    action: Action::default(),
                },
            );
        }
    }

    /// Defaults for a brand-new profile (also used by "clear all data").
    pub fn fresh() -> Config {
        let mut cfg = Config {
            wheels: vec![Wheel { items: Vec::new() }],
            ..Config::default()
        };
        cfg.ensure_list_entry();
        cfg
    }

    pub fn route_index(&self) -> usize {
        match self.route {
            Route::List => self.wheels.len(),
            Route::Wheel(i) => i.min(self.wheels.len().saturating_sub(1)),
        }
    }

    pub fn set_route_index(&mut self, idx: usize) {
        if idx >= self.wheels.len() {
            self.route = Route::List;
        } else {
            self.route = Route::Wheel(idx);
        }
    }
}

/// Logic checks for id / reserved-key allocation; run with NOKEYS_SELFTEST=1.
#[cfg(debug_assertions)]
pub fn selftest() -> Vec<(&'static str, bool)> {
    fn item(id: u32, action: Action) -> Item {
        Item {
            id,
            name: format!("功能 {id}"),
            icon: String::new(),
            is_list_entry: false,
            vk: None,
            action,
        }
    }
    /// Profile with exactly these ids, all sending their reserved key.
    fn profile(ids: &[u32]) -> Config {
        let mut c = Config::fresh();
        c.wheels[0].items.clear();
        for id in ids {
            c.wheels[0].items.push(item(*id, Action::default()));
        }
        c.normalize_vks();
        c
    }
    fn vk_of(c: &Config, id: u32) -> Option<u16> {
        c.items().find(|i| i.id == id).and_then(|i| i.vk)
    }

    let mut out = Vec::new();

    // --- ids ---
    out.push(("id: 1..9 minus 7 -> 7", profile(&[1, 2, 3, 4, 5, 6, 8, 9]).free_id() == 7));
    out.push(("id: 1 2 4 -> 3", profile(&[1, 2, 4]).free_id() == 3));
    out.push(("id: 1..3 -> 4", profile(&[1, 2, 3]).free_id() == 4));
    out.push(("id: no gaps -> 1", profile(&[]).free_id() == 1));
    {
        let mut c = profile(&[1, 2, 3]);
        c.wheels[0].items.retain(|i| i.id != 2);
        let id = c.free_id();
        let si = c.wheels[0].items.len();
        c.wheels[0].items.push(item(id, Action::default()));
        c.sync_vk(0, si);
        out.push(("id: delete 2 then add -> 2", id == 2 && vk_of(&c, 2).is_some()));
    }

    // --- reserved keys ---
    let c = profile(&[1, 2, 3]);
    out.push((
        "vk: first three -> F13 F14 F15",
        vk_of(&c, 1) == Some(VK_POOL[0]) && vk_of(&c, 2) == Some(VK_POOL[1]) && vk_of(&c, 3) == Some(VK_POOL[2]),
    ));
    {
        // Delete the item holding F14, then add: the new item gets F14 back.
        let mut c = profile(&[1, 2, 3]);
        c.wheels[0].items.retain(|i| i.id != 2);
        let id = c.free_id();
        let si = c.wheels[0].items.len();
        c.wheels[0].items.push(item(id, Action::default()));
        c.sync_vk(0, si);
        out.push(("vk: freed key is reused", vk_of(&c, id) == Some(VK_POOL[1])));
    }
    {
        // Switching the action away from the reserved key hands it back.
        let mut c = profile(&[1, 2]);
        c.wheels[0].items[0].action = Action::Program {
            path: "notepad.exe".into(),
            args: String::new(),
        };
        c.sync_vk(0, 0);
        out.push(("vk: unused key released", vk_of(&c, 1).is_none() && c.free_vk() == VK_POOL[0]));
    }
    {
        // An existing profile keeps the key its id used to produce (id 15 -> F15).
        let c = profile(&[15]);
        out.push(("vk: legacy id keeps its key", vk_of(&c, 15) == Some(VK_POOL[2]) && vk_name(VK_POOL[2]) == "F15"));
    }
    {
        let mut c = profile(&[1, 2]);
        c.wheels[0].items[1].action = Action::Text {
            text: "hi".into(),
        };
        c.normalize_vks();
        out.push(("vk: text item holds no key", vk_of(&c, 2).is_none()));
    }
    {
        // The fixed list entry never emits a key, so it must not consume one.
        let mut c = Config::fresh();
        c.wheels[0].items.push(item(2, Action::default()));
        c.normalize_vks();
        out.push((
            "vk: list entry holds no key",
            vk_of(&c, 1).is_none() && vk_of(&c, 2).is_some(),
        ));
    }
    {
        // A key freed at runtime goes to the lowest gap, not back to the id map.
        let mut c = profile(&[5, 6]);
        c.wheels[0].items[1].action = Action::Program {
            path: "notepad.exe".into(),
            args: String::new(),
        };
        c.sync_vk(0, 1);
        let si = c.wheels[0].items.len();
        c.wheels[0].items.push(item(9, Action::default()));
        c.sync_vk(0, si);
        out.push((
            "vk: new item takes the lowest free key",
            vk_of(&c, 9) == Some(VK_POOL[0]) && vk_of(&c, 5) == Some(VK_POOL[4]),
        ));
    }
    out
}
