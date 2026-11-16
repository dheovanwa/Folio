use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_custom_bg")]
    pub custom_bg: String,
    #[serde(default = "default_custom_fg")]
    pub custom_fg: String,
    #[serde(default = "default_custom_accent")]
    pub custom_accent: String,
    #[serde(default = "default_font_family")]
    pub font_family: String,
    #[serde(default = "default_font_size")]
    pub font_size: f64,
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
}

#[allow(dead_code)]
pub type FolioConfig = Config;
#[allow(dead_code)]
pub type TypographyConfig = Config;

fn default_theme() -> String {
    "light".into()
}
fn default_custom_bg() -> String {
    "#ffffff".into()
}
fn default_custom_fg() -> String {
    "#1a1a1a".into()
}
fn default_custom_accent() -> String {
    "#3584e4".into()
}
fn default_font_family() -> String {
    "Times New Roman".into()
}
fn default_font_size() -> f64 {
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

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            custom_bg: default_custom_bg(),
            custom_fg: default_custom_fg(),
            custom_accent: default_custom_accent(),
            font_family: default_font_family(),
            font_size: default_font_size(),
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
                Ok(contents) => match toml::from_str(&contents) {
                    Ok(config) => return config,
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

    /// Check if a hex color code represents a dark background using ITU-R BT.601 luma.
    pub fn is_color_dark(hex: &str) -> bool {
        let hex = hex.trim().trim_start_matches('#');
        if hex.len() == 6 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&hex[0..2], 16),
                u8::from_str_radix(&hex[2..4], 16),
                u8::from_str_radix(&hex[4..6], 16),
            ) {
                let lum = 0.299 * (r as f64) + 0.587 * (g as f64) + 0.114 * (b as f64);
                return lum < 128.0;
            }
        }
        false
    }

    pub fn is_dark(&self) -> bool {
        match self.theme.as_str() {
            "dark" | "charcoal" | "oled" => true,
            "custom" => Self::is_color_dark(&self.custom_bg),
            _ => false,
        }
    }

    pub fn toggle_dark_mode(&mut self) {
        if self.is_dark() {
            self.theme = "light".to_string();
        } else {
            self.theme = "dark".to_string();
        }
    }

    #[allow(dead_code)]
    pub fn cycle_theme(&mut self) {
        let presets = Self::theme_presets();
        let current_idx = presets.iter().position(|&t| t == self.theme).unwrap_or(0);
        let next_idx = (current_idx + 1) % presets.len();
        self.theme = presets[next_idx].to_string();
    }

    pub fn bg_color(&self) -> &str {
        match self.theme.as_str() {
            "light" => "#faf8f5",
            "sepia" => "#f4ecd8",
            "dark" => "#1e1e2e",
            "charcoal" => "#161616",
            "oled" => "#000000",
            "custom" => &self.custom_bg,
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
            "custom" => &self.custom_fg,
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
            "custom" => &self.custom_accent,
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
            "custom" => "#888888",
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
            "custom" => {
                if self.is_dark() {
                    "#1c1c1c"
                } else {
                    "#f0ede8"
                }
            }
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
            "custom" => {
                if self.is_dark() {
                    "#e0e0e0"
                } else {
                    "#1a1a1a"
                }
            }
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
        assert_eq!(config.font_family, "Times New Roman");
        assert_eq!(config.font_size, 18.0);
        assert_eq!(config.line_height, 1.7);
        assert_eq!(config.max_width, 720.0);
        assert_eq!(config.margin, 32.0);
        assert!(config.show_progress_bar);
        assert!(config.auto_detect_clipboard);
        assert!(config.smooth_scrolling);
        assert_eq!(config.scroll_step_px, 120.0);
        assert_eq!(config.paragraph_spacing_px, 16);
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

        config.theme = "charcoal".into();
        assert!(config.is_dark());
        config.toggle_dark_mode();
        assert_eq!(config.theme, "light");

        config.theme = "oled".into();
        assert!(config.is_dark());
        config.toggle_dark_mode();
        assert_eq!(config.theme, "light");
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

        config.theme = "custom".into();
        config.custom_bg = "#123456".into();
        config.custom_fg = "#00ff00".into();
        assert_eq!(config.bg_color(), "#123456");
        assert_eq!(config.fg_color(), "#00ff00");
        // Custom theme headerbar does NOT take custom_fg
        assert_eq!(config.headerbar_fg(), "#e0e0e0");
        assert_ne!(config.headerbar_fg(), config.custom_fg);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let config = Config::default();
        let serialized = toml::to_string(&config).expect("serialize config");
        let deserialized: Config = toml::from_str(&serialized).expect("deserialize config");
        assert_eq!(config.theme, deserialized.theme);
        assert_eq!(config.font_size, deserialized.font_size);
    }
}
