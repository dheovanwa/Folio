use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use crate::config::Config;

/// Build the settings sidebar content for the OverlaySplitView.
///
/// Returns the sidebar widget and a sync callback that updates UI controls
/// when configuration changes externally (e.g. via keyboard shortcut or headerbar toggle).
pub fn build_settings_sidebar(
    config: Rc<RefCell<Config>>,
    on_config_changed: Rc<dyn Fn()>,
    on_close_sidebar: Rc<dyn Fn()>,
) -> (gtk::Box, Rc<dyn Fn()>) {
    let sidebar = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .width_request(320)
        .build();
    sidebar.add_css_class("settings-sidebar");

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .build();

    let inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(8)
        .margin_bottom(16)
        .margin_start(8)
        .margin_end(8)
        .build();

    let is_syncing = Rc::new(Cell::new(false));

    // Debounced config persistence helper to prevent synchronous disk I/O while dragging sliders
    let save_timeout: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let request_save: Rc<dyn Fn()> = {
        let config = config.clone();
        let save_timeout = save_timeout.clone();
        Rc::new(move || {
            if let Some(src) = save_timeout.borrow_mut().take() {
                src.remove();
            }
            let cfg = config.clone();
            let st = save_timeout.clone();
            let src_id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(250),
                move || {
                    st.borrow_mut().take();
                    cfg.borrow().save();
                },
            );
            *save_timeout.borrow_mut() = Some(src_id);
        })
    };

    // ── Section: Header with Close Button ──
    let header_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .hexpand(true)
        .build();

    let header_label = gtk::Label::builder()
        .label("Reading Preferences")
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();
    header_label.add_css_class("title-3");

    let close_btn = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Close settings (Ctrl+,)")
        .build();
    close_btn.add_css_class("flat");
    {
        let close_cb = on_close_sidebar.clone();
        let cfg = config.clone();
        let st = save_timeout.clone();
        close_btn.connect_clicked(move |_| {
            if let Some(src) = st.borrow_mut().take() {
                src.remove();
            }
            cfg.borrow().save();
            close_cb();
        });
    }

    header_box.append(&header_label);
    header_box.append(&close_btn);
    inner.append(&header_box);

    // ── Section: Color Theme ──
    let theme_title = section_title("Appearance & Color Theme");
    inner.append(&theme_title);

    // Dark Mode quick switch
    let dark_mode_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_bottom(6)
        .build();

    let dark_mode_label = gtk::Label::builder()
        .label("Dark Mode")
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();

    let dark_mode_switch = gtk::Switch::builder()
        .active(config.borrow().is_dark())
        .valign(gtk::Align::Center)
        .tooltip_text("Toggle dark/light mode (t)")
        .build();

    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        let is_syncing_switch = is_syncing.clone();
        dark_mode_switch.connect_active_notify(move |sw| {
            if is_syncing_switch.get() {
                return;
            }
            let active = sw.is_active();
            let is_currently_dark = cfg.borrow().is_dark();
            if active != is_currently_dark {
                cfg.borrow_mut().toggle_dark_mode();
                cfg.borrow().save();
                changed();
            }
        });
    }

    dark_mode_box.append(&dark_mode_label);
    dark_mode_box.append(&dark_mode_switch);
    inner.append(&dark_mode_box);

    // Theme Palette Buttons
    let theme_palette_label = gtk::Label::builder()
        .label("Theme Presets")
        .halign(gtk::Align::Start)
        .build();
    theme_palette_label.add_css_class("dim-label");
    theme_palette_label.add_css_class("caption");
    inner.append(&theme_palette_label);

    let theme_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Start)
        .build();

    let theme_buttons: Vec<(&str, &str)> = vec![
        ("light", "theme-btn-light"),
        ("sepia", "theme-btn-sepia"),
        ("dark", "theme-btn-dark"),
        ("oled", "theme-btn-oled"),
        ("custom", "theme-btn-custom"),
    ];

    let theme_toggle_group: Rc<RefCell<Vec<gtk::ToggleButton>>> = Rc::new(RefCell::new(Vec::new()));

    let mut first_btn: Option<gtk::ToggleButton> = None;
    for (theme_name, css_class) in &theme_buttons {
        let btn = gtk::ToggleButton::new();
        btn.add_css_class("theme-btn");
        btn.add_css_class(css_class);
        btn.set_tooltip_text(Some(&capitalize(theme_name)));

        if first_btn.is_some() {
            btn.set_group(first_btn.as_ref());
        }

        if config.borrow().theme == *theme_name {
            btn.set_active(true);
        }

        let cfg = config.clone();
        let changed = on_config_changed.clone();
        let name = theme_name.to_string();
        let is_syncing_btn = is_syncing.clone();
        btn.connect_toggled(move |b| {
            if is_syncing_btn.get() {
                return;
            }
            if b.is_active() {
                cfg.borrow_mut().theme = name.clone();
                cfg.borrow().save();
                changed();
            }
        });

        theme_box.append(&btn);
        if first_btn.is_none() {
            first_btn = Some(btn.clone());
        }
        theme_toggle_group.borrow_mut().push(btn);
    }
    inner.append(&theme_box);

    // ── Custom Colors (only meaningful when theme = custom) ──
    let custom_section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();

    let custom_label = section_title("Custom Colors");
    custom_section.append(&custom_label);

    let custom_bg_row = color_row("Background", &config.borrow().custom_bg, {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        move |color| {
            cfg.borrow_mut().custom_bg = color;
            cfg.borrow().save();
            changed();
        }
    });
    custom_section.append(&custom_bg_row);

    let custom_fg_row = color_row("Text", &config.borrow().custom_fg, {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        move |color| {
            cfg.borrow_mut().custom_fg = color;
            cfg.borrow().save();
            changed();
        }
    });
    custom_section.append(&custom_fg_row);

    let custom_accent_row = color_row("Accent", &config.borrow().custom_accent, {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        move |color| {
            cfg.borrow_mut().custom_accent = color;
            cfg.borrow().save();
            changed();
        }
    });
    custom_section.append(&custom_accent_row);
    inner.append(&custom_section);

    // ── Section: Typography ──
    let typo_title = section_title("Typography & Layout");
    inner.append(&typo_title);

    // Font family
    let font_row = font_picker_row("Font Family", &config.borrow().font_family, {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        move |val| {
            cfg.borrow_mut().font_family = val;
            cfg.borrow().save();
            changed();
        }
    });
    inner.append(&font_row);

    // Font size slider
    let font_size_row = labeled_scale(
        "Font Size",
        14.0,
        32.0,
        1.0,
        config.borrow().font_size,
        "px",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            move |val| {
                cfg.borrow_mut().font_size = val;
                save();
                changed();
            }
        },
    );
    inner.append(&font_size_row);

    // Line height slider
    let line_height_row = labeled_scale(
        "Line Height",
        1.2,
        2.2,
        0.1,
        config.borrow().line_height,
        "",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            move |val| {
                cfg.borrow_mut().line_height = (val * 10.0).round() / 10.0;
                save();
                changed();
            }
        },
    );
    inner.append(&line_height_row);

    // Letter spacing slider
    let letter_row = labeled_scale(
        "Letter Spacing",
        -1.0,
        3.0,
        0.1,
        config.borrow().letter_spacing,
        "px",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            move |val| {
                cfg.borrow_mut().letter_spacing = (val * 10.0).round() / 10.0;
                save();
                changed();
            }
        },
    );
    inner.append(&letter_row);

    // Max column width
    let width_row = labeled_scale(
        "Max Width",
        550.0,
        950.0,
        10.0,
        config.borrow().max_width,
        "px",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            move |val| {
                cfg.borrow_mut().max_width = val.round();
                save();
                changed();
            }
        },
    );
    inner.append(&width_row);

    // Text margin slider
    let margin_row = labeled_scale(
        "Text Margin",
        12.0,
        120.0,
        4.0,
        config.borrow().margin,
        "px",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            move |val| {
                cfg.borrow_mut().margin = val.round();
                save();
                changed();
            }
        },
    );
    inner.append(&margin_row);

    // Text alignment toggle
    let align_title = gtk::Label::builder()
        .label("Text Alignment")
        .halign(gtk::Align::Start)
        .build();
    inner.append(&align_title);

    let align_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .build();

    let align_left = gtk::ToggleButton::builder().label("Left").build();
    let align_justify = gtk::ToggleButton::builder().label("Justified").build();
    align_justify.set_group(Some(&align_left));

    if config.borrow().justified {
        align_justify.set_active(true);
    } else {
        align_left.set_active(true);
    }

    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        align_left.connect_toggled(move |b| {
            if b.is_active() {
                cfg.borrow_mut().justified = false;
                cfg.borrow().save();
                changed();
            }
        });
    }
    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        align_justify.connect_toggled(move |b| {
            if b.is_active() {
                cfg.borrow_mut().justified = true;
                cfg.borrow().save();
                changed();
            }
        });
    }

    align_box.append(&align_left);
    align_box.append(&align_justify);
    inner.append(&align_box);

    // ── Section: Focus Aids ──
    let focus_title = section_title("Comfort & Focus");
    inner.append(&focus_title);

    // Progress bar toggle
    let progress_check = gtk::CheckButton::builder()
        .label("Show Reading Progress Bar")
        .active(config.borrow().show_progress_bar)
        .build();
    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        progress_check.connect_toggled(move |b| {
            cfg.borrow_mut().show_progress_bar = b.is_active();
            cfg.borrow().save();
            changed();
        });
    }
    inner.append(&progress_check);

    // Auto-scroll speed
    let scroll_row = labeled_scale(
        "Auto-Scroll Speed",
        10.0,
        120.0,
        5.0,
        config.borrow().auto_scroll_speed,
        "px/s",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            move |val| {
                cfg.borrow_mut().auto_scroll_speed = val.round();
                save();
                changed();
            }
        },
    );
    inner.append(&scroll_row);

    let scroll_hint = gtk::Label::builder()
        .label("Press s to toggle auto-scroll")
        .halign(gtk::Align::Start)
        .build();
    scroll_hint.add_css_class("dim-label");
    scroll_hint.add_css_class("caption");
    inner.append(&scroll_hint);

    // Clipboard auto-detect
    let clipboard_check = gtk::CheckButton::builder()
        .label("Auto-Detect Clipboard URLs")
        .active(config.borrow().auto_detect_clipboard)
        .build();
    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        clipboard_check.connect_toggled(move |b| {
            cfg.borrow_mut().auto_detect_clipboard = b.is_active();
            cfg.borrow().save();
            changed();
        });
    }
    inner.append(&clipboard_check);

    // ── Keyboard Shortcuts Reference ──
    let kb_title = section_title("Keyboard Shortcuts");
    inner.append(&kb_title);

    let shortcuts = [
        ("Ctrl+L", "Focus URL bar"),
        ("Ctrl+,", "Toggle settings"),
        ("j / k", "Smooth scroll down / up"),
        ("Space", "Scroll down page (browser)"),
        ("Shift+Space", "Scroll up page (browser)"),
        ("s", "Toggle auto-scroll"),
        ("t", "Toggle dark/light mode"),
        ("F11", "Zen mode (fullscreen)"),
    ];
    for (key, desc) in shortcuts {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let key_label = gtk::Label::builder()
            .label(key)
            .width_chars(10)
            .halign(gtk::Align::Start)
            .build();
        key_label.add_css_class("monospace");
        key_label.add_css_class("dim-label");
        let desc_label = gtk::Label::builder()
            .label(desc)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        row.append(&key_label);
        row.append(&desc_label);
        inner.append(&row);
    }

    // ── Section: Configuration File ──
    let config_title = section_title("Configuration File");
    inner.append(&config_title);

    let config_path = Config::config_path();
    let config_path_str = config_path.display().to_string();
    let config_label = gtk::Label::builder()
        .label(&config_path_str)
        .halign(gtk::Align::Start)
        .wrap(true)
        .selectable(true)
        .build();
    config_label.add_css_class("dim-label");
    config_label.add_css_class("caption");
    config_label.add_css_class("monospace");
    inner.append(&config_label);

    scroller.set_child(Some(&inner));
    sidebar.append(&scroller);

    let sync_sidebar: Rc<dyn Fn()> = {
        let config = config.clone();
        let is_syncing = is_syncing.clone();
        let dark_mode_switch = dark_mode_switch.clone();
        let theme_toggle_group = theme_toggle_group.clone();
        let theme_names: Vec<String> = theme_buttons.iter().map(|(n, _)| n.to_string()).collect();

        Rc::new(move || {
            is_syncing.set(true);
            let cfg = config.borrow();
            dark_mode_switch.set_active(cfg.is_dark());
            let btns = theme_toggle_group.borrow();
            for (idx, name) in theme_names.iter().enumerate() {
                if let Some(btn) = btns.get(idx) {
                    btn.set_active(&cfg.theme == name);
                }
            }
            is_syncing.set(false);
        })
    };

    (sidebar, sync_sidebar)
}

fn section_title(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .margin_top(12)
        .build();
    label.add_css_class("settings-section-title");
    label
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
}

fn labeled_scale(
    label_text: &str,
    min: f64,
    max: f64,
    step: f64,
    initial: f64,
    unit: &str,
    on_change: impl Fn(f64) + 'static,
) -> gtk::Box {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();

    let unit_owned = unit.to_string();
    let value_text = if unit.is_empty() {
        format!("{label_text}: {initial:.1}")
    } else {
        format!("{label_text}: {initial:.0}{unit_owned}")
    };

    let label = gtk::Label::builder()
        .label(&value_text)
        .halign(gtk::Align::Start)
        .build();

    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
    scale.set_value(initial);
    scale.set_hexpand(true);
    scale.set_draw_value(false);

    let label_clone = label.clone();
    let label_text_owned = label_text.to_string();

    // 16ms frame-throttling to keep UI running at 60 FPS without event backlog
    let pending_source: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let latest_val = Rc::new(Cell::new(initial));
    let on_change = Rc::new(on_change);

    scale.connect_value_changed(move |s| {
        let val = s.value();
        let text = if unit_owned.is_empty() {
            format!("{label_text_owned}: {val:.1}")
        } else {
            format!("{label_text_owned}: {val:.0}{unit_owned}")
        };
        label_clone.set_label(&text);
        latest_val.set(val);

        if pending_source.borrow().is_none() {
            let ps = pending_source.clone();
            let lv = latest_val.clone();
            let oc = on_change.clone();
            let source_id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(16),
                move || {
                    ps.borrow_mut().take();
                    oc(lv.get());
                },
            );
            *pending_source.borrow_mut() = Some(source_id);
        }
    });

    container.append(&label);
    container.append(&scale);
    container
}

fn font_picker_row(
    label_text: &str,
    initial_font: &str,
    on_change: impl Fn(String) + 'static,
) -> gtk::Box {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();

    let label = gtk::Label::builder()
        .label(label_text)
        .halign(gtk::Align::Start)
        .build();

    let dialog = gtk::FontDialog::builder()
        .title("Choose Reading Font")
        .modal(true)
        .build();

    let font_btn = gtk::FontDialogButton::builder()
        .dialog(&dialog)
        .use_font(true)
        .use_size(false)
        .hexpand(true)
        .build();

    let initial_desc = gtk::pango::FontDescription::from_string(initial_font);
    font_btn.set_font_desc(&initial_desc);

    font_btn.connect_font_desc_notify(move |btn| {
        if let Some(desc) = btn.font_desc() {
            if let Some(family) = desc.family() {
                on_change(family.to_string());
            }
        }
    });

    container.append(&label);
    container.append(&font_btn);
    container
}


fn color_row(
    label_text: &str,
    initial_hex: &str,
    on_change: impl Fn(String) + 'static,
) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();

    let label = gtk::Label::builder()
        .label(label_text)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();

    let color_btn = gtk::ColorDialogButton::builder().build();
    let rgba = gtk::gdk::RGBA::parse(initial_hex).unwrap_or(gtk::gdk::RGBA::BLACK);
    color_btn.set_rgba(&rgba);

    let dialog = gtk::ColorDialog::builder()
        .title(label_text)
        .modal(true)
        .build();
    color_btn.set_dialog(&dialog);

    color_btn.connect_rgba_notify(move |btn| {
        let rgba = btn.rgba();
        let hex = format!(
            "#{:02x}{:02x}{:02x}",
            (rgba.red() * 255.0) as u8,
            (rgba.green() * 255.0) as u8,
            (rgba.blue() * 255.0) as u8,
        );
        on_change(hex);
    });

    row.append(&label);
    row.append(&color_btn);
    row
}
