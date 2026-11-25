use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Global configuration, persisted as JSON.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Config {
    pub hotkey: Hotkey,
    pub route: Route,
    pub theme: Theme,
    pub blur: Blur,
    pub list_background: ListBackground,
    /// Colour of the floating cards in the list view (`#RRGGBB`).
    #[serde(default = "default_card_color")]
    pub card_color: String,
    /// 0.0 .. 1.0 opacity of the list view cards.
    #[serde(default = "default_card_opacity")]
    pub card_opacity: f32,
    pub animation: Animation,
    pub wheels: Vec<Wheel>,
    pub next_id: u32,
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
pub enum Theme {
    Auto,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Blur {
    pub enabled: bool,
    /// 0.0 .. 1.0, strength of the frosted glass (tint opacity / blur radius).
    pub strength: f32,
    /// Tint color, `#RRGGBB`.
    pub color: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ListBackground {
    pub enabled: bool,
    /// Background color, `#RRGGBB`.
    pub color: String,
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
    /// nokeys_N id, monotonically increasing, never reused.
    pub id: u32,
    /// User-visible function name.
    pub name: String,
    /// Path to icon file (empty = default glyph).
    pub icon: String,
    /// True for the fixed "open list view" entry (only in wheel #1).
    pub is_list_entry: bool,
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
            theme: Theme::Auto,
            blur: Blur {
                enabled: true,
                strength: 0.5,
                color: "#1f2430".into(),
            },
            list_background: ListBackground {
                enabled: false,
                color: "#1e1e2e".into(),
                image: String::new(),
                opacity: 0.5,
            },
            card_color: "#22242e".into(),
            card_opacity: 0.85,
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
                        },
                        Item {
                            id: 2,
                            name: "示例功能A".into(),
                            icon: String::new(),
                            is_list_entry: false,
                        },
                        Item {
                            id: 3,
                            name: "示例功能B".into(),
                            icon: String::new(),
                            is_list_entry: false,
                        },
                        Item {
                            id: 4,
                            name: "示例功能C".into(),
                            icon: String::new(),
                            is_list_entry: false,
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
                        },
                        Item {
                            id: 6,
                            name: "示例功能E".into(),
                            icon: String::new(),
                            is_list_entry: false,
                        },
                    ],
                },
            ],
            next_id: 7,
        }
    }
}

fn default_card_color() -> String {
    "#22242e".into()
}

fn default_card_opacity() -> f32 {
    0.85
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
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("config parse error ({e}), using defaults");
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(icons_dir())?;
    let text = serde_json::to_string_pretty(cfg).expect("serialize config");
    std::fs::write(config_path(), text)
}

impl Config {
    /// Allocate a fresh, never-reused nokeys id.
    pub fn alloc_id(&mut self) -> u32 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        id
    }

    /// Wheel #0 always carries exactly one fixed "list view" entry at slot 0.
    pub fn ensure_list_entry(&mut self) {
        if self.wheels.is_empty() {
            self.wheels.push(Wheel { items: Vec::new() });
        }
        let has_entry = self.wheels[0].items.iter().any(|i| i.is_list_entry);
        if !has_entry {
            let id = self.alloc_id();
            self.wheels[0].items.insert(
                0,
                Item {
                    id,
                    name: "列表视图".into(),
                    icon: String::new(),
                    is_list_entry: true,
                },
            );
        }
    }

    /// Defaults for a brand-new profile (also used by "clear all data").
    pub fn fresh() -> Config {
        let mut cfg = Config {
            wheels: vec![Wheel { items: Vec::new() }],
            next_id: 1,
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
