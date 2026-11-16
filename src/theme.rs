use crate::config::Config;

/// Generate CSS string from the current configuration for the GTK4 CssProvider.
pub fn generate_css(config: &Config) -> String {
    let bg = config.bg_color();
    let fg = config.fg_color();
    let accent = config.accent_color();
    let meta = config.meta_color();
    let hdr_bg = config.headerbar_bg();
    let font = &config.font_family;
    let font_size = config.font_size as i32;
    let line_height = config.line_height;
    let letter_spacing = config.letter_spacing;
    let margin = config.margin as i32;

    format!(
        r#"
/* ── Global ── */
window, .main-box {{
    background-color: {bg};
    color: {fg};
}}

headerbar {{
    background-color: {hdr_bg};
    color: {fg};
    border-bottom: none;
    box-shadow: none;
}}

headerbar entry {{
    background-color: alpha({fg}, 0.08);
    color: {fg};
    border-radius: 8px;
    min-height: 32px;
}}

headerbar button {{
    color: {fg};
}}

/* ── Reading Progress ── */
.reading-progress {{
    min-height: 3px;
    border-radius: 0;
}}
.reading-progress trough {{
    min-height: 3px;
    background-color: transparent;
    border-radius: 0;
}}
.reading-progress progress {{
    min-height: 3px;
    background-color: {accent};
    border-radius: 0;
}}

/* ── Article Container ── */
.article-viewport {{
    background-color: {bg};
}}

.article-content {{
    background-color: {bg};
    color: {fg};
    font-family: "{font}";
    font-size: {font_size}px;
    padding: 40px {margin}px 120px {margin}px;
    line-height: {line_height};
    letter-spacing: {letter_spacing}px;
}}

.article-wrapper {{
    background-color: {bg};
}}

.article-title {{
    font-size: {title_size}px;
    font-weight: 800;
    color: {fg};
    margin-bottom: 8px;
    line-height: 1.2;
}}

.article-byline {{
    font-size: {meta_size}px;
    color: {meta};
    margin-bottom: 4px;
}}

.article-reading-time {{
    font-size: {meta_small_size}px;
    color: {meta};
    margin-bottom: 32px;
}}

.article-body {{
    font-size: {font_size}px;
    color: {fg};
    line-height: {line_height};
    letter-spacing: {letter_spacing}px;
}}

.article-body-text {{
    font-size: {font_size}px;
    color: {fg};
    line-height: {line_height};
    letter-spacing: {letter_spacing}px;
}}

.article-h2 {{
    font-size: {h2_size}px;
    font-weight: 700;
    color: {fg};
    margin-top: 24px;
    margin-bottom: 8px;
    line-height: 1.3;
}}

.article-h3 {{
    font-size: {h3_size}px;
    font-weight: 600;
    color: {fg};
    margin-top: 18px;
    margin-bottom: 6px;
    line-height: 1.3;
}}

.article-paragraph {{
    font-size: {font_size}px;
    color: {fg};
    line-height: {line_height};
    letter-spacing: {letter_spacing}px;
    margin-bottom: 16px;
}}

.article-blockquote {{
    border-left: 3px solid {accent};
    padding-left: 16px;
    margin-top: 12px;
    margin-bottom: 16px;
}}

.article-blockquote-text {{
    font-style: italic;
    font-size: {font_size}px;
    color: {meta};
    line-height: {line_height};
}}

.article-codeblock {{
    background-color: alpha({fg}, 0.07);
    border-radius: 6px;
    padding: 12px 16px;
    margin-top: 10px;
    margin-bottom: 16px;
    font-family: monospace;
    font-size: {code_size}px;
}}

.article-list-item {{
    font-size: {font_size}px;
    color: {fg};
    line-height: {line_height};
    margin-bottom: 6px;
    margin-left: 16px;
}}

/* ── Max Width Wrapper ── */
.max-width-clamp {{
    background-color: transparent;
}}

/* ── Empty State ── */
.empty-state {{
    color: {meta};
}}

.empty-state-icon {{
    margin-bottom: 12px;
    color: alpha({fg}, 0.25);
}}

.empty-state-title {{
    font-size: 22px;
    font-weight: 700;
    color: {fg};
    margin-bottom: 6px;
}}

.empty-state-subtitle {{
    font-size: 14px;
    color: {meta};
}}

/* ── Spinner ── */
.loading-spinner {{
    color: {accent};
}}

/* ── Settings Sidebar ── */
.settings-sidebar {{
    background-color: {hdr_bg};
    color: {fg};
    padding: 16px;
    border-left: 1px solid alpha({fg}, 0.1);
}}

.settings-sidebar label {{
    color: {fg};
}}

.settings-sidebar scale trough {{
    background-color: alpha({fg}, 0.12);
}}

.settings-sidebar scale highlight {{
    background-color: {accent};
}}

.settings-section-title {{
    font-weight: 700;
    font-size: 13px;
    color: {meta};
    margin-top: 16px;
    margin-bottom: 6px;
}}

/* ── Theme Buttons ── */
.theme-btn {{
    min-width: 36px;
    min-height: 36px;
    border-radius: 20px;
    padding: 0;
    border: 2px solid alpha({fg}, 0.15);
}}
.theme-btn:checked {{
    border: 3px solid {accent};
}}
.theme-btn-light {{
    background-color: #faf8f5;
}}
.theme-btn-sepia {{
    background-color: #f4ecd8;
}}
.theme-btn-dark {{
    background-color: #1e1e2e;
}}
.theme-btn-oled {{
    background-color: #000000;
}}
.theme-btn-custom {{
    background: linear-gradient(135deg, #ff6b6b, #4ecdc4, #45b7d1);
}}

/* ── Library Popover ── */
.library-list {{
    background-color: {bg};
}}

.library-row {{
    padding: 8px 12px;
    border-bottom: 1px solid alpha({fg}, 0.06);
}}

.library-row-title {{
    font-weight: 600;
    font-size: 14px;
    color: {fg};
}}

.library-row-url {{
    font-size: 11px;
    color: {meta};
}}

/* ── Link Colors ── */
link, .article-body link {{
    color: {accent};
}}
"#,
        title_size = (config.font_size * 1.8) as i32,
        h2_size = (config.font_size * 1.35) as i32,
        h3_size = (config.font_size * 1.15) as i32,
        code_size = (config.font_size * 0.9) as i32,
        meta_size = (config.font_size * 0.85) as i32,
        meta_small_size = (config.font_size * 0.75) as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_css_contains_theme_values() {
        let config = Config::default();
        let css = generate_css(&config);
        assert!(css.contains(".article-content"));
        assert!(css.contains("#faf8f5"));
        assert!(css.contains("font-size: 18px"));
    }

    #[test]
    fn test_generate_css_dark_theme() {
        let mut config = Config::default();
        config.theme = "dark".into();
        let css = generate_css(&config);
        assert!(css.contains("#1e1e2e"));
    }

    #[test]
    fn test_generate_css_custom_theme() {
        let mut config = Config::default();
        config.theme = "custom".into();
        config.custom_bg = "#112233".into();
        config.custom_fg = "#445566".into();
        config.custom_accent = "#778899".into();
        let css = generate_css(&config);
        assert!(css.contains("#112233"));
        assert!(css.contains("#445566"));
        assert!(css.contains("#778899"));
    }

    #[test]
    fn test_gtk_css_provider_validity() {
        if gtk::init().is_ok() {
            let provider = gtk::CssProvider::new();
            for theme_name in &["light", "sepia", "dark", "oled", "custom"] {
                let mut config = Config::default();
                config.theme = (*theme_name).to_string();
                let css = generate_css(&config);
                provider.load_from_string(&css);
            }
        }
    }
}
