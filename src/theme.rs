use crate::config::Config;

/// Generate CSS string from the current configuration for the GTK4 CssProvider.
pub fn generate_css(config: &Config) -> String {
    let bg = config.bg_color();
    let fg = config.fg_color();
    let accent = config.accent_color();
    let meta = config.meta_color();
    let hdr_bg = config.headerbar_bg();
    let hdr_fg = config.headerbar_fg();
    let clean_font = config.font_family.trim().trim_matches('"').trim_matches('\'').trim();
    let clean_font = if clean_font.is_empty() { "Times New Roman" } else { clean_font };
    let font_size = config.font_size as i32;
    // Chrome scale. Every previously hard-coded chrome size (titles, labels,
    // library rows, nav buttons, empty state) is expressed as a multiple of
    // `ui_font_size` so one slider scales the whole interface coherently
    // instead of leaving 13px/15px/22px islands behind.
    let ui = config.ui_font_size.max(6.0);
    let ui_small = (ui * 0.85).round().max(6.0);
    let ui_tiny = (ui * 0.79).round().max(6.0);
    let ui_nav = (ui * 1.07).round().max(6.0);
    let ui_title = (ui * 1.57).round().max(6.0);
    let ui_section = (ui * 0.93).round().max(6.0);
    let toc_font = config.toc_font_size.max(6.0);
    let line_height = config.line_height;
    let letter_spacing = config.letter_spacing;
    let margin = config.margin as i32;
    let paragraph_spacing = config.paragraph_spacing_px;

    format!(
        r#"
/* ── Global ── */
window, .main-box {{
    background-color: {bg};
    color: {fg};
}}

headerbar {{
    background-color: {hdr_bg};
    color: {hdr_fg};
    border-bottom: none;
    box-shadow: none;
    font-size: {ui}px;
}}

headerbar entry {{
    background-color: alpha({hdr_fg}, 0.08);
    color: {hdr_fg};
    border-radius: 8px;
    min-height: 32px;
    font-size: {ui}px;
}}

headerbar button {{
    color: {hdr_fg};
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
.article-viewport, .reader-surface {{
    background-color: {bg};
}}

.article-box, .article-list-view {{
    background-color: {bg};
    padding-top: 40px;
    padding-bottom: 120px;
    padding-left: {margin}px;
    padding-right: {margin}px;
}}

/* ── TextView Article Surface ── */
textview.article-textview,
textview.article-textview text {{
    background-color: {bg};
    color: {fg};
    font-family: "{clean_font}", serif;
    font-size: {font_size}px;
    line-height: {line_height};
    letter-spacing: {letter_spacing}px;
    border: none;
    outline: none;
    box-shadow: none;
}}

textview.article-textview text selection {{
    background-color: alpha({accent}, 0.35);
    color: {fg};
}}

/* ── Table of Contents Sidebar ── */
.toc-sidebar {{
    background-color: {bg};
    border-right: 1px solid alpha({fg}, 0.08);
    padding: 12px 0 12px 0;
}}

.toc-title {{
    font-size: {ui_small}px;
    font-weight: 700;
    letter-spacing: 0.5px;
    color: {meta};
    padding: 4px 16px 10px 16px;
}}

.toc-scroll {{
    background-color: transparent;
}}

.toc-list,
.toc-list row,
.toc-list > row {{
    background-color: transparent;
    border: none;
    outline: none;
    box-shadow: none;
    padding: 0;
    margin: 0;
    border-radius: 6px;
}}

.toc-list row:hover {{
    background-color: alpha({fg}, 0.05);
}}

.toc-label {{
    font-family: "{clean_font}", serif;
    font-size: {toc_font}px;
    color: {fg};
    padding: 6px 12px 6px 16px;
}}

.toc-level-1 {{
    font-weight: 600;
}}

.toc-level-2,
.toc-level-3,
.toc-level-4 {{
    font-weight: 400;
    color: {meta};
}}

.toc-empty {{
    font-size: {ui_small}px;
    font-style: italic;
    color: {meta};
    padding: 6px 16px;
}}

/* ── In-Page Search Bar ── */
.article-search-bar {{
    background-color: {hdr_bg};
    border-bottom: 1px solid alpha({hdr_fg}, 0.1);
    padding: 4px 12px;
}}

.article-search-bar entry {{
    background-color: alpha({hdr_fg}, 0.08);
    color: {hdr_fg};
    border-radius: 8px;
    min-height: 32px;
}}

.search-count-label {{
    font-size: {ui_small}px;
    font-weight: 600;
    margin-left: 6px;
    margin-right: 6px;
    color: {meta};
}}

listview.article-list-view,
listview.article-list-view row,
listview.article-list-view > row {{
    background-color: transparent;
    border: none;
    outline: none;
    box-shadow: none;
    padding: 0;
    margin: 0;
}}

listview.article-list-view row:hover,
listview.article-list-view row:selected,
listview.article-list-view row:focus {{
    background-color: transparent;
}}

.article-block-container {{
    padding-left: {margin}px;
    padding-right: {margin}px;
}}

/* ── Typography & Explicit Label Targeting ── */
.reader-surface,
.reader-surface label,
.article-viewport,
.article-viewport label,
.article-box,
.article-box label,
.article-list-view,
.article-list-view label,
listview.article-list-view > row label,
.article-block-container label,
.article-content,
.article-content label,
.article-title,
.article-byline,
.article-reading-time,
.article-heading,
.article-h2,
.article-h3,
.article-paragraph,
.article-body-text,
.article-quote,
.article-blockquote,
.article-list-item,
.article-divider {{
    font-family: "{clean_font}", serif;
}}

.article-code,
.article-codeblock,
.article-code label,
.article-codeblock label {{
    font-family: monospace;
}}

.article-content {{
    background-color: {bg};
    color: {fg};
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
    margin-bottom: {paragraph_spacing}px;
}}

.article-blockquote,
.article-quote {{
    border-left: 3px solid {accent};
    padding-left: 16px;
    margin-top: 12px;
    margin-bottom: {paragraph_spacing}px;
    font-style: italic;
    font-size: {font_size}px;
    color: {meta};
    line-height: {line_height};
}}

.article-codeblock,
.article-code {{
    background-color: alpha({fg}, 0.07);
    border-radius: 6px;
    padding: 12px 16px;
    margin-top: 10px;
    margin-bottom: {paragraph_spacing}px;
    font-size: {code_size}px;
}}

.article-list-item {{
    font-size: {font_size}px;
    color: {fg};
    line-height: {line_height};
    margin-bottom: 6px;
    margin-left: 16px;
}}

.article-divider {{
    font-size: {font_size}px;
    color: {meta};
    margin-top: 18px;
    margin-bottom: 24px;
    letter-spacing: 4px;
}}

/* ── Reader Navigation Row & Buttons ── */
.reader-nav-row {{
    margin-top: 24px;
    margin-bottom: 36px;
}}

.reader-nav-btn {{
    padding: 10px 22px;
    font-size: {ui_nav}px;
    font-weight: 600;
    border-radius: 9999px;
    background-color: alpha({accent}, 0.12);
    color: {accent};
    border: 1px solid alpha({accent}, 0.25);
    transition: all 180ms ease;
}}

.reader-nav-btn:hover {{
    background-color: alpha({accent}, 0.24);
    border-color: {accent};
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
    font-size: {ui_title}px;
    font-weight: 700;
    color: {fg};
    margin-bottom: 6px;
}}

.empty-state-subtitle {{
    font-size: {ui}px;
    color: {meta};
}}

/* ── Spinner ── */
.loading-spinner {{
    color: {accent};
}}

/* ── Settings Sidebar ── */
.settings-sidebar {{
    background-color: {hdr_bg};
    color: {hdr_fg};
    padding: 16px;
    border-left: 1px solid alpha({hdr_fg}, 0.1);
    font-family: initial;
    font-size: {ui}px;
}}

.settings-sidebar label {{
    color: {hdr_fg};
    font-family: initial;
}}

.settings-sidebar label.monospace {{
    font-family: monospace;
}}

.settings-sidebar preferencesgroup label,
.settings-sidebar comborow label,
.settings-sidebar actionrow label,
.settings-sidebar row label,
.settings-sidebar listview row label,
.settings-sidebar listbox row label {{
    font-family: initial;
}}

.settings-sidebar scale trough {{
    background-color: alpha({hdr_fg}, 0.12);
}}

.settings-sidebar scale highlight {{
    background-color: {accent};
}}

.settings-section-title {{
    font-weight: 700;
    font-size: {ui_section}px;
    color: {meta};
    margin-top: 16px;
    margin-bottom: 6px;
}}

.config-path-btn {{
    padding: 6px 8px;
    border-radius: 8px;
    transition: background-color 150ms ease-in-out;
}}

.config-path-btn:hover {{
    background-color: alpha({hdr_fg}, 0.08);
}}

/* ── Theme Buttons ── */
.theme-btn {{
    min-width: 36px;
    min-height: 36px;
    border-radius: 20px;
    padding: 0;
    border: 2px solid alpha({hdr_fg}, 0.15);
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
.theme-btn-charcoal {{
    background-color: #161616;
}}
.theme-btn-oled {{
    background-color: #000000;
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
    font-size: {ui}px;
    color: {fg};
}}

.library-row-url {{
    font-size: {ui_tiny}px;
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

/// Alias for generate_css
#[allow(dead_code)]
pub fn generate_gtk_css(config: &Config) -> String {
    generate_css(config)
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
        assert!(css.contains("Times New Roman"));
        assert!(css.contains("margin-bottom: 16px"));
    }

    #[test]
    fn test_generate_css_custom_font_and_spacing() {
        let mut config = Config::default();
        config.font_family = "Inter".into();
        config.paragraph_spacing_px = 24;
        let css = generate_gtk_css(&config);
        assert!(css.contains("font-family: \"Inter\", serif;"));
        assert!(css.contains("margin-bottom: 24px;"));
    }

    #[test]
    fn test_settings_sidebar_font_isolation() {
        let mut config = Config::default();
        config.font_family = "Comic Sans MS".into();
        let css = generate_css(&config);

        // General listview > row label should NOT be present (preventing leakage into preferences rows)
        assert!(!css.contains("listview > row label,"));

        // Settings sidebar and comborows must be isolated with font-family: initial
        assert!(css.contains(".settings-sidebar"));
        assert!(css.contains(".settings-sidebar preferencesgroup label"));
        assert!(css.contains(".settings-sidebar comborow label"));
        assert!(css.contains("font-family: initial;"));
    }

    #[test]
    fn test_ui_and_toc_font_sizes_are_configurable() {
        let mut config = Config::default();
        config.ui_font_size = 18.0;
        config.toc_font_size = 20.0;
        let css = generate_css(&config);

        // Contents list honours its own size exactly.
        assert!(
            css.contains(".toc-label {\n    font-family: \"Times New Roman\", serif;\n    font-size: 20px;"),
            "toc label must use toc_font_size:\n{css}"
        );
        // Chrome scales: 18 -> 18, 0.85*18=15.3 -> 15, 0.79*18=14.2 -> 14,
        // 1.07*18=19.3 -> 19, 1.57*18=28.3 -> 28, 0.93*18=16.7 -> 17
        assert!(css.contains(".empty-state-subtitle {\n    font-size: 18px;"));
        assert!(css.contains(".empty-state-title {\n    font-size: 28px;"));
        assert!(css.contains(".reader-nav-btn {\n    padding: 10px 22px;\n    font-size: 19px;"));
        assert!(css.contains(".settings-section-title {\n    font-weight: 700;\n    font-size: 17px;"));
        assert!(
            css.contains(".library-row-url {\n    font-size: 14px;"),
            "library url must track ui_font_size:\n{css}"
        );
        // The article body is untouched by ui_font_size.
        assert!(css.contains(".article-content {\n    background-color: #faf8f5;\n    color: #1a1a1a;\n    font-size: 18px;"));
    }

    #[test]
    fn test_ui_font_size_does_not_change_article_typography() {
        let mut base = Config::default();
        base.ui_font_size = 16.0;
        let mut changed = Config::default();
        changed.ui_font_size = 22.0;

        let extract = |css: &str| -> String {
            let start = css.find(".article-content {").unwrap();
            css[start..css[start..].find('}').unwrap() + start].to_string()
        };
        assert_eq!(extract(&generate_css(&base)), extract(&generate_css(&changed)));
    }

    #[test]
    fn test_toc_font_size_does_not_leak_into_article() {
        let mut base = Config::default();
        base.toc_font_size = 12.0;
        let mut changed = Config::default();
        changed.toc_font_size = 24.0;
        let a = generate_css(&base);
        let b = generate_css(&changed);
        // Only the toc rules differ.
        let count_toc = |css: &str| css.matches(".toc-label {\n    font-family: \"Times New Roman\", serif;\n    font-size: 12px;").count();
        assert_eq!(count_toc(&a), 1);
        assert_eq!(a.replace("font-size: 12px;\n    color: #1a1a1a;\n    padding: 6px 12px", ""), b.replace("font-size: 24px;\n    color: #1a1a1a;\n    padding: 6px 12px", ""));
    }

    #[test]
    fn test_generate_css_dark_theme() {
        let mut config = Config::default();
        config.theme = "dark".into();
        let css = generate_css(&config);
        assert!(css.contains("#1e1e2e"));
    }

    #[test]
    fn test_generate_css_charcoal_theme() {
        let mut config = Config::default();
        config.theme = "charcoal".into();
        let css = generate_css(&config);
        assert!(css.contains("#161616"));
        assert!(css.contains("#e4e4e4"));
    }

    #[test]
    fn test_gtk_css_provider_validity() {
        // GTK parsing lives on the shared GTK owner thread; the CSS strings
        // themselves are pure, so they are built without any widget access.
        let raws: Vec<(String, String)> = ["light", "sepia", "dark", "charcoal", "oled"]
            .iter()
            .map(|theme_name| {
                let mut config = Config::default();
                config.theme = (*theme_name).to_string();
                ((*theme_name).to_string(), generate_css(&config))
            })
            .collect();
        for (theme_name, css) in &raws {
            assert!(!css.trim().is_empty(), "{theme_name} produced empty CSS");
        }

        if crate::test_support::with_gtk(move || {
            let provider = gtk::CssProvider::new();
            for (theme_name, css) in raws {
                provider.load_from_string(&css);
                let _ = theme_name;
            }
        })
        .is_none()
        {
            crate::test_support::skip_if_no_display();
        }
    }



}
