use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_light_theme")]
    pub light_theme: String,
    #[serde(default = "default_dark_theme")]
    pub dark_theme: String,
    #[serde(default = "default_font_family")]
    pub font_family: String,
    #[serde(default = "default_font_size")]
    pub font_size: f64,
    /// Font size of the application chrome (header bar, settings drawer,
    /// library rows, empty state, navigation buttons) in CSS pixels.
    #[serde(default = "default_ui_font_size")]
    pub ui_font_size: f64,
    /// Font size of the Table of Contents sidebar entries in CSS pixels.
    #[serde(default = "default_toc_font_size")]
    pub toc_font_size: f64,
    #[serde(default = "default_line_height")]
    pub line_height: f64,
    #[serde(default = "default_letter_spacing")]
    pub letter_spacing: f64,
    #[serde(default = "default_max_width")]
    pub max_width: f64,
    #[serde(default = "default_margin")]
    pub margin: f64,
    #[serde(default = "default_justified")]
    pub justified: bool,
    #[serde(default = "default_auto_scroll_speed")]
    pub auto_scroll_speed: f64,
    #[serde(default = "default_auto_detect_clipboard")]
    pub auto_detect_clipboard: bool,
    #[serde(default = "default_show_progress_bar")]
    pub show_progress_bar: bool,
    #[serde(default = "default_smooth_scrolling")]
    pub smooth_scrolling: bool,
    #[serde(default = "default_scroll_step_px")]
    pub scroll_step_px: f64,
    #[serde(default = "default_paragraph_spacing_px")]
    #[serde(alias = "paragraph_spacing")]
    pub paragraph_spacing_px: u32,
    /// Whether the Table of Contents sidebar is shown.
    #[serde(default = "default_show_toc")]
    pub show_toc: bool,
    #[serde(default)]
    pub window: WindowConfig,
    #[serde(default = "default_startup_behavior")]
    pub startup_behavior: StartupBehavior,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StartupBehavior {
    #[default]
    #[serde(rename = "blank_page", alias = "BlankPage")]
    BlankPage,
    #[serde(rename = "recent_page", alias = "RecentPage")]
    RecentPage,
}

fn default_startup_behavior() -> StartupBehavior {
    StartupBehavior::BlankPage
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowConfig {
    #[serde(default = "default_window_width")]
    pub width: i32,
    #[serde(default = "default_window_height")]
    pub height: i32,
    #[serde(default = "default_window_is_maximized")]
    pub is_maximized: bool,
    #[serde(default = "default_window_is_fullscreen")]
    pub is_fullscreen: bool,
}

fn default_window_width() -> i32 {
    900
}
fn default_window_height() -> i32 {
    720
}
fn default_window_is_maximized() -> bool {
    false
}
fn default_window_is_fullscreen() -> bool {
    false
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: default_window_width(),
            height: default_window_height(),
            is_maximized: default_window_is_maximized(),
            is_fullscreen: default_window_is_fullscreen(),
        }
    }
}

#[allow(dead_code)]
pub type FolioConfig = Config;
#[allow(dead_code)]
pub type TypographyConfig = Config;
#[allow(dead_code)]
pub type GeneralConfig = Config;

fn default_theme() -> String {
    "light".into()
}
fn default_light_theme() -> String {
    "light".into()
}
fn default_dark_theme() -> String {
    "dark".into()
}
fn default_font_family() -> String {
    "Times New Roman".into()
}
fn default_font_size() -> f64 {
    18.0
}
fn default_ui_font_size() -> f64 {
    20.0
}
fn default_toc_font_size() -> f64 {
    18.0
}
fn default_line_height() -> f64 {
    1.7
}
fn default_letter_spacing() -> f64 {
    0.0
}
fn default_max_width() -> f64 {
    720.0
}
fn default_margin() -> f64 {
    32.0
}
fn default_justified() -> bool {
    false
}
fn default_auto_scroll_speed() -> f64 {
    30.0
}
fn default_auto_detect_clipboard() -> bool {
    true
}
fn default_show_progress_bar() -> bool {
    true
}
fn default_smooth_scrolling() -> bool {
    true
}
fn default_scroll_step_px() -> f64 {
    120.0
}
fn default_paragraph_spacing_px() -> u32 {
    16
}
fn default_show_toc() -> bool {
    false
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            light_theme: default_light_theme(),
            dark_theme: default_dark_theme(),
            font_family: default_font_family(),
            font_size: default_font_size(),
            ui_font_size: default_ui_font_size(),
            toc_font_size: default_toc_font_size(),
            line_height: default_line_height(),
            letter_spacing: default_letter_spacing(),
            max_width: default_max_width(),
            margin: default_margin(),
            justified: default_justified(),
            auto_scroll_speed: default_auto_scroll_speed(),
            auto_detect_clipboard: default_auto_detect_clipboard(),
            show_progress_bar: default_show_progress_bar(),
            smooth_scrolling: default_smooth_scrolling(),
            scroll_step_px: default_scroll_step_px(),
            paragraph_spacing_px: default_paragraph_spacing_px(),
            show_toc: default_show_toc(),
            window: WindowConfig::default(),
            startup_behavior: default_startup_behavior(),
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("folio");
        config_dir.join("config.toml")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if path.exists() {
            match fs::read_to_string(&path) {
                Ok(contents) => match toml::from_str::<Config>(&contents) {
                    Ok(mut config) => {
                        if config.theme == "custom" {
                            config.theme = "light".to_string();
                        }
                        if config.light_theme == "custom" {
                            config.light_theme = "light".to_string();
                        }
                        if config.dark_theme == "custom" {
                            config.dark_theme = "dark".to_string();
                        }
                        if !contents.contains("light_theme") && config.theme == "sepia" {
                            config.light_theme = "sepia".to_string();
                        }
                        if !contents.contains("dark_theme")
                            && (config.theme == "charcoal" || config.theme == "oled")
                        {
                            config.dark_theme = config.theme.clone();
                        }
                        return config;
                    }
                    Err(e) => eprintln!("folio: failed to parse config: {e}"),
                },
                Err(e) => eprintln!("folio: failed to read config: {e}"),
            }
        }
        let config = Self::default();
        config.save();
        config
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match toml::to_string_pretty(self) {
            Ok(contents) => {
                if let Err(e) = fs::write(&path, contents) {
                    eprintln!("folio: failed to write config: {e}");
                }
            }
            Err(e) => eprintln!("folio: failed to serialize config: {e}"),
        }
    }

    #[allow(dead_code)]
    pub fn theme_presets() -> &'static [&'static str] {
        &["light", "sepia", "dark", "charcoal", "oled"]
    }

    #[allow(dead_code)]
    pub fn light_theme_presets() -> &'static [&'static str] {
        &["light", "sepia"]
    }

    #[allow(dead_code)]
    pub fn dark_theme_presets() -> &'static [&'static str] {
        &["dark", "charcoal", "oled"]
    }

    pub fn set_theme(&mut self, theme: &str) {
        self.theme = theme.to_string();
        match theme {
            "light" | "sepia" => {
                self.light_theme = theme.to_string();
            }
            "dark" | "charcoal" | "oled" => {
                self.dark_theme = theme.to_string();
            }
            _ => {}
        }
    }

    pub fn set_light_theme(&mut self, theme: &str) {
        self.light_theme = theme.to_string();
        if !self.is_dark() {
            self.theme = self.light_theme.clone();
        }
    }

    pub fn set_dark_theme(&mut self, theme: &str) {
        self.dark_theme = theme.to_string();
        if self.is_dark() {
            self.theme = self.dark_theme.clone();
        }
    }

    pub fn is_dark(&self) -> bool {
        matches!(self.theme.as_str(), "dark" | "charcoal" | "oled")
    }

    pub fn toggle_dark_mode(&mut self) {
        if self.is_dark() {
            self.theme = self.light_theme.clone();
        } else {
            self.theme = self.dark_theme.clone();
        }
    }

    #[allow(dead_code)]
    pub fn cycle_theme(&mut self) {
        let presets = Self::theme_presets();
        let current_idx = presets.iter().position(|&t| t == self.theme).unwrap_or(0);
        let next_idx = (current_idx + 1) % presets.len();
        self.set_theme(presets[next_idx]);
    }

    pub fn bg_color(&self) -> &str {
        match self.theme.as_str() {
            "light" => "#faf8f5",
            "sepia" => "#f4ecd8",
            "dark" => "#1e1e2e",
            "charcoal" => "#161616",
            "oled" => "#000000",
            _ => "#faf8f5",
        }
    }

    pub fn fg_color(&self) -> &str {
        match self.theme.as_str() {
            "light" => "#1a1a1a",
            "sepia" => "#5b4636",
            "dark" => "#cdd6f4",
            "charcoal" => "#e4e4e4",
            "oled" => "#e0e0e0",
            _ => "#1a1a1a",
        }
    }

    pub fn accent_color(&self) -> &str {
        match self.theme.as_str() {
            "light" => "#3584e4",
            "sepia" => "#8b6914",
            "dark" => "#89b4fa",
            "charcoal" => "#5dade2",
            "oled" => "#bb86fc",
            _ => "#3584e4",
        }
    }

    pub fn meta_color(&self) -> &str {
        match self.theme.as_str() {
            "light" => "#6b7280",
            "sepia" => "#8b7355",
            "dark" => "#a6adc8",
            "charcoal" => "#9a9a9a",
            "oled" => "#999999",
            _ => "#6b7280",
        }
    }

    pub fn headerbar_bg(&self) -> &str {
        match self.theme.as_str() {
            "light" => "#f0ede8",
            "sepia" => "#e8dfc8",
            "dark" => "#181825",
            "charcoal" => "#1c1c1c",
            "oled" => "#000000",
            _ => "#f0ede8",
        }
    }

    pub fn headerbar_fg(&self) -> &str {
        match self.theme.as_str() {
            "light" => "#1a1a1a",
            "sepia" => "#5b4636",
            "dark" => "#cdd6f4",
            "charcoal" => "#e4e4e4",
            "oled" => "#e0e0e0",
            _ => "#1a1a1a",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.theme, "light");
        assert_eq!(config.light_theme, "light");
        assert_eq!(config.dark_theme, "dark");
        assert_eq!(config.font_family, "Times New Roman");
        assert_eq!(config.font_size, 18.0);
        assert_eq!(config.ui_font_size, 20.0);
        assert_eq!(config.toc_font_size, 18.0);
        assert_eq!(config.line_height, 1.7);
        assert_eq!(config.max_width, 720.0);
        assert_eq!(config.margin, 32.0);
        assert!(config.show_progress_bar);
        assert!(config.auto_detect_clipboard);
        assert!(config.smooth_scrolling);
        assert_eq!(config.scroll_step_px, 120.0);
        assert_eq!(config.paragraph_spacing_px, 16);
        assert!(!config.show_toc);
        assert_eq!(config.window.width, 900);
        assert_eq!(config.window.height, 720);
        assert!(!config.window.is_maximized);
        assert!(!config.window.is_fullscreen);
        assert_eq!(config.startup_behavior, StartupBehavior::BlankPage);
    }

    #[test]
    fn test_cycle_theme() {
        let mut config = Config::default();
        assert_eq!(config.theme, "light");
        config.cycle_theme();
        assert_eq!(config.theme, "sepia");
        config.cycle_theme();
        assert_eq!(config.theme, "dark");
        config.cycle_theme();
        assert_eq!(config.theme, "charcoal");
        config.cycle_theme();
        assert_eq!(config.theme, "oled");
        config.cycle_theme();
        assert_eq!(config.theme, "light");
    }

    #[test]
    fn test_dark_mode_toggle() {
        let mut config = Config::default();
        assert!(!config.is_dark());
        config.toggle_dark_mode();
        assert_eq!(config.theme, "dark");
        assert!(config.is_dark());
        config.toggle_dark_mode();
        assert_eq!(config.theme, "light");
        assert!(!config.is_dark());

        // When charcoal or oled preset is set as dark_theme
        config.set_dark_theme("charcoal");
        assert_eq!(config.dark_theme, "charcoal");
        assert_eq!(config.theme, "light");
        config.toggle_dark_mode();
        assert_eq!(config.theme, "charcoal");
        assert!(config.is_dark());
        config.toggle_dark_mode();
        assert_eq!(config.theme, "light");

        // When sepia preset is set as light_theme
        config.set_light_theme("sepia");
        assert_eq!(config.light_theme, "sepia");
        assert_eq!(config.theme, "sepia");
        config.toggle_dark_mode();
        assert_eq!(config.theme, "charcoal");
        config.toggle_dark_mode();
        assert_eq!(config.theme, "sepia");

        // When oled preset is set as dark_theme
        config.set_dark_theme("oled");
        assert_eq!(config.dark_theme, "oled");
        config.toggle_dark_mode();
        assert_eq!(config.theme, "oled");
    }

    #[test]
    fn test_light_and_dark_theme_presets() {
        let mut config = Config::default();
        assert_eq!(Config::light_theme_presets(), &["light", "sepia"]);
        assert_eq!(Config::dark_theme_presets(), &["dark", "charcoal", "oled"]);

        // Selecting a theme via set_theme automatically updates corresponding preset
        config.set_theme("sepia");
        assert_eq!(config.theme, "sepia");
        assert_eq!(config.light_theme, "sepia");

        config.set_theme("charcoal");
        assert_eq!(config.theme, "charcoal");
        assert_eq!(config.dark_theme, "charcoal");

        config.set_theme("oled");
        assert_eq!(config.theme, "oled");
        assert_eq!(config.dark_theme, "oled");

        // Toggling switches between the chosen presets (sepia <-> oled)
        config.toggle_dark_mode();
        assert_eq!(config.theme, "sepia");
        config.toggle_dark_mode();
        assert_eq!(config.theme, "oled");
    }

    #[test]
    fn test_color_accessors() {
        let mut config = Config::default();
        assert_eq!(config.bg_color(), "#faf8f5");
        assert_eq!(config.fg_color(), "#1a1a1a");
        assert_eq!(config.headerbar_bg(), "#f0ede8");
        assert_eq!(config.headerbar_fg(), "#1a1a1a");

        config.theme = "charcoal".into();
        assert_eq!(config.bg_color(), "#161616");
        assert_eq!(config.fg_color(), "#e4e4e4");
        assert_eq!(config.headerbar_bg(), "#1c1c1c");
        assert_eq!(config.headerbar_fg(), "#e4e4e4");

        config.theme = "oled".into();
        assert_eq!(config.bg_color(), "#000000");
    }

    #[test]
    fn test_custom_theme_fallback_from_toml() {
        let toml_custom = r##"
theme = "custom"
light_theme = "custom"
dark_theme = "custom"
custom_bg = "#123456"
"##;
        let mut cfg: Config = toml::from_str(toml_custom).expect("deserialize custom config");
        if cfg.theme == "custom" {
            cfg.theme = "light".to_string();
        }
        if cfg.light_theme == "custom" {
            cfg.light_theme = "light".to_string();
        }
        if cfg.dark_theme == "custom" {
            cfg.dark_theme = "dark".to_string();
        }
        assert_eq!(cfg.theme, "light");
        assert_eq!(cfg.light_theme, "light");
        assert_eq!(cfg.dark_theme, "dark");
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut config = Config::default();
        config.window.width = 1024;
        config.window.height = 768;
        config.window.is_maximized = true;
        config.startup_behavior = StartupBehavior::RecentPage;
        let serialized = toml::to_string(&config).expect("serialize config");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize config");
        assert_eq!(config.theme, deserialized.theme);
        assert_eq!(config.light_theme, deserialized.light_theme);
        assert_eq!(config.dark_theme, deserialized.dark_theme);
        assert_eq!(config.font_size, deserialized.font_size);
        assert_eq!(config.window, deserialized.window);
        assert_eq!(config.startup_behavior, deserialized.startup_behavior);
    }

    #[test]
    fn test_startup_behavior_deserialization() {
        // Test default when omitted
        let toml_empty = "";
        let cfg: Config = toml::from_str(toml_empty).expect("deserialize empty config");
        assert_eq!(cfg.startup_behavior, StartupBehavior::BlankPage);

        // Test snake_case
        let toml_recent = r#"startup_behavior = "recent_page""#;
        let cfg: Config = toml::from_str(toml_recent).expect("deserialize recent_page");
        assert_eq!(cfg.startup_behavior, StartupBehavior::RecentPage);

        let toml_blank = r#"startup_behavior = "blank_page""#;
        let cfg: Config = toml::from_str(toml_blank).expect("deserialize blank_page");
        assert_eq!(cfg.startup_behavior, StartupBehavior::BlankPage);

        // Test PascalCase aliases
        let toml_recent_pascal = r#"startup_behavior = "RecentPage""#;
        let cfg: Config = toml::from_str(toml_recent_pascal).expect("deserialize RecentPage");
        assert_eq!(cfg.startup_behavior, StartupBehavior::RecentPage);

        let toml_blank_pascal = r#"startup_behavior = "BlankPage""#;
        let cfg: Config = toml::from_str(toml_blank_pascal).expect("deserialize BlankPage");
        assert_eq!(cfg.startup_behavior, StartupBehavior::BlankPage);
    }
}
