use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::config::Config;

/// Build the settings sidebar content for the OverlaySplitView.
///
/// Returns the sidebar widget and a sync callback that updates UI controls
/// when configuration changes externally (e.g. via keyboard shortcut or headerbar toggle).
pub fn build_settings_sidebar(
    config: Rc<RefCell<Config>>,
    on_config_changed: Rc<dyn Fn()>,
    on_close_sidebar: Rc<dyn Fn()>,
    on_show_shortcuts: Rc<dyn Fn()>,
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

    // Debounced config persistence helper (350ms) to eliminate disk I/O lag while modifying settings
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
                std::time::Duration::from_millis(350),
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
        let save = request_save.clone();
        let is_syncing_switch = is_syncing.clone();
        dark_mode_switch.connect_active_notify(move |sw| {
            if is_syncing_switch.get() {
                return;
            }
            let active = sw.is_active();
            let is_currently_dark = cfg.borrow().is_dark();
            if active != is_currently_dark {
                cfg.borrow_mut().toggle_dark_mode();
                save();
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
        ("charcoal", "theme-btn-charcoal"),
        ("oled", "theme-btn-oled"),
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
        let save = request_save.clone();
        let name = theme_name.to_string();
        let is_syncing_btn = is_syncing.clone();
        btn.connect_toggled(move |b| {
            if is_syncing_btn.get() {
                return;
            }
            if b.is_active() {
                cfg.borrow_mut().set_theme(&name);
                save();
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

    // ── Light & Dark Mode Preset Selection ──
    let light_theme_model = gtk::StringList::new(&["Light", "Sepia"]);
    let light_preset_row = adw::ComboRow::builder()
        .title("Light Theme Preset")
        .subtitle("Preset used when Light Mode is active")
        .model(&light_theme_model)
        .selected(match config.borrow().light_theme.as_str() {
            "sepia" => 1,
            _ => 0,
        })
        .build();

    let dark_theme_model = gtk::StringList::new(&["Dark", "Charcoal", "OLED"]);
    let dark_preset_row = adw::ComboRow::builder()
        .title("Dark Theme Preset")
        .subtitle("Preset used when Dark Mode is active")
        .model(&dark_theme_model)
        .selected(match config.borrow().dark_theme.as_str() {
            "charcoal" => 1,
            "oled" => 2,
            _ => 0,
        })
        .build();

    let theme_preset_group = adw::PreferencesGroup::new();
    theme_preset_group.set_margin_top(8);
    theme_preset_group.set_margin_bottom(6);
    theme_preset_group.add(&light_preset_row);
    theme_preset_group.add(&dark_preset_row);
    inner.append(&theme_preset_group);

    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        let save = request_save.clone();
        let is_syncing_row = is_syncing.clone();
        light_preset_row.connect_selected_notify(move |row| {
            if is_syncing_row.get() {
                return;
            }
            let chosen = match row.selected() {
                1 => "sepia",
                _ => "light",
            };
            cfg.borrow_mut().set_light_theme(chosen);
            save();
            changed();
        });
    }

    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        let save = request_save.clone();
        let is_syncing_row = is_syncing.clone();
        dark_preset_row.connect_selected_notify(move |row| {
            if is_syncing_row.get() {
                return;
            }
            let chosen = match row.selected() {
                1 => "charcoal",
                2 => "oled",
                _ => "dark",
            };
            cfg.borrow_mut().set_dark_theme(chosen);
            save();
            changed();
        });
    }

    // ── Section: Typography ──
    let typo_title = section_title("Typography & Layout");
    inner.append(&typo_title);

    // Font family
    let font_row = font_picker_row("Font Family", &config.borrow().font_family, {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        let save = request_save.clone();
        move |val| {
            cfg.borrow_mut().font_family = val;
            save();
            changed();
        }
    });
    inner.append(&font_row);

    // Font size slider
    let font_size_row = labeled_scale(
        "Article Font Size",
        10.0,
        48.0,
        1.0,
        config.borrow().font_size,
        "px",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            let is_syncing = is_syncing.clone();
            move |val| {
                if is_syncing.get() {
                    return;
                }
                cfg.borrow_mut().font_size = val;
                save();
                changed();
            }
        },
    );
    let font_size_scale = font_size_row.last_child().and_downcast::<gtk::Scale>();
    inner.append(&font_size_row);

    // Interface (chrome) font size — header bar, settings drawer, library
    // rows, empty state, navigation buttons.
    let ui_font_row = labeled_scale(
        "Interface Font Size",
        9.0,
        40.0,
        1.0,
        config.borrow().ui_font_size,
        "px",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            let is_syncing = is_syncing.clone();
            move |val| {
                if is_syncing.get() {
                    return;
                }
                cfg.borrow_mut().ui_font_size = val;
                save();
                changed();
            }
        },
    );
    let ui_font_scale = ui_font_row.last_child().and_downcast::<gtk::Scale>();
    inner.append(&ui_font_row);

    // Contents sidebar font size.
    let toc_font_row = labeled_scale(
        "Contents Sidebar Size",
        9.0,
        40.0,
        1.0,
        config.borrow().toc_font_size,
        "px",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            let is_syncing = is_syncing.clone();
            move |val| {
                if is_syncing.get() {
                    return;
                }
                cfg.borrow_mut().toc_font_size = val;
                save();
                changed();
            }
        },
    );
    let toc_font_scale = toc_font_row.last_child().and_downcast::<gtk::Scale>();
    inner.append(&toc_font_row);

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

    // Paragraph Spacing slider (4px - 48px, step 2px)
    let para_spacing_row = labeled_scale(
        "Paragraph Spacing",
        4.0,
        48.0,
        2.0,
        config.borrow().paragraph_spacing_px as f64,
        "px",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            move |val| {
                cfg.borrow_mut().paragraph_spacing_px = val.round() as u32;
                save();
                changed();
            }
        },
    );
    inner.append(&para_spacing_row);

    // Max column width (expanded: 320px - 1400px for ultrawide and broad margins)
    let width_row = labeled_scale(
        "Max Width",
        320.0,
        1400.0,
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

    // Text margin slider (expanded 3x upper limit: 12px - 360px)
    let margin_row = labeled_scale(
        "Text Margin",
        12.0,
        360.0,
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
        let save = request_save.clone();
        align_left.connect_toggled(move |b| {
            if b.is_active() {
                cfg.borrow_mut().justified = false;
                save();
                changed();
            }
        });
    }
    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        let save = request_save.clone();
        align_justify.connect_toggled(move |b| {
            if b.is_active() {
                cfg.borrow_mut().justified = true;
                save();
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

    // Smooth Scrolling toggle switch
    let smooth_scroll_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_bottom(4)
        .build();

    let smooth_scroll_label = gtk::Label::builder()
        .label("Smooth Scrolling")
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();

    let smooth_scroll_switch = gtk::Switch::builder()
        .active(config.borrow().smooth_scrolling)
        .valign(gtk::Align::Center)
        .tooltip_text("Enable frame-synced smooth scrolling for keyboard navigation")
        .build();

    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        let save = request_save.clone();
        let is_syncing_switch = is_syncing.clone();
        smooth_scroll_switch.connect_active_notify(move |sw| {
            if is_syncing_switch.get() {
                return;
            }
            cfg.borrow_mut().smooth_scrolling = sw.is_active();
            save();
            changed();
        });
    }

    smooth_scroll_box.append(&smooth_scroll_label);
    smooth_scroll_box.append(&smooth_scroll_switch);
    inner.append(&smooth_scroll_box);

    // Scroll step slider
    let step_row = labeled_scale(
        "Scroll Step",
        40.0,
        300.0,
        10.0,
        config.borrow().scroll_step_px,
        "px",
        {
            let cfg = config.clone();
            let changed = on_config_changed.clone();
            let save = request_save.clone();
            move |val| {
                cfg.borrow_mut().scroll_step_px = val.round();
                save();
                changed();
            }
        },
    );
    inner.append(&step_row);

    // Progress bar toggle
    let progress_check = gtk::CheckButton::builder()
        .label("Show Reading Progress Bar")
        .active(config.borrow().show_progress_bar)
        .build();
    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        let save = request_save.clone();
        progress_check.connect_toggled(move |b| {
            cfg.borrow_mut().show_progress_bar = b.is_active();
            save();
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
        let save = request_save.clone();
        clipboard_check.connect_toggled(move |b| {
            cfg.borrow_mut().auto_detect_clipboard = b.is_active();
            save();
            changed();
        });
    }
    inner.append(&clipboard_check);

    // ── Section: General ──
    let general_title = section_title("General");
    inner.append(&general_title);

    let startup_model = gtk::StringList::new(&["Blank Page", "Recent Page"]);
    let startup_row = adw::ComboRow::builder()
        .title("On Startup")
        .subtitle("Choose what to display when Folio opens")
        .model(&startup_model)
        .selected(match config.borrow().startup_behavior {
            crate::config::StartupBehavior::BlankPage => 0,
            crate::config::StartupBehavior::RecentPage => 1,
        })
        .build();

    let general_group = adw::PreferencesGroup::new();
    general_group.add(&startup_row);
    inner.append(&general_group);

    {
        let cfg = config.clone();
        let changed = on_config_changed.clone();
        let save = request_save.clone();
        let is_syncing_row = is_syncing.clone();
        startup_row.connect_selected_notify(move |row| {
            if is_syncing_row.get() {
                return;
            }
            let behavior = match row.selected() {
                1 => crate::config::StartupBehavior::RecentPage,
                _ => crate::config::StartupBehavior::BlankPage,
            };
            cfg.borrow_mut().startup_behavior = behavior;
            save();
            changed();
        });
    }

    // ── Keyboard Shortcuts Reference ──
    let kb_title = section_title("Keyboard Shortcuts");
    inner.append(&kb_title);

    let shortcuts_btn = gtk::Button::builder()
        .label("View All Shortcuts (Ctrl+? / F1)")
        .icon_name("preferences-desktop-keyboard-shortcuts-symbolic")
        .tooltip_text("Open keyboard shortcuts dialog (Ctrl+? / F1)")
        .halign(gtk::Align::Fill)
        .build();
    shortcuts_btn.add_css_class("flat");
    {
        let show_sc = on_show_shortcuts.clone();
        shortcuts_btn.connect_clicked(move |_| {
            show_sc();
        });
    }
    inner.append(&shortcuts_btn);

    let shortcuts = [
        ("Ctrl+? / F1", "Show keyboard shortcuts"),
        ("Ctrl+L", "Focus URL bar"),
        ("Ctrl+F", "Find in article"),
        ("F3 / Shift+F3", "Next / previous search match"),
        ("Ctrl+C", "Copy selected text"),
        ("Ctrl+A", "Select all article text"),
        ("Ctrl+R / F5", "Refresh article"),
        ("Ctrl+,", "Toggle settings"),
        ("Ctrl++ / Ctrl+-", "Zoom in / out"),
        ("Ctrl+0", "Reset zoom (100%)"),
        ("Alt+← / Alt+→", "History back / forward"),
        ("j / k, ↓ / ↑", "Smooth scroll down / up"),
        ("Space", "Scroll down page (browser)"),
        ("Shift+Space / Backspace", "Scroll up page (browser)"),
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

    let config_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .hexpand(true)
        .build();

    let config_btn = gtk::Button::builder()
        .tooltip_text("Click to open in default file manager")
        .hexpand(true)
        .build();
    config_btn.set_cursor_from_name(Some("pointer"));
    config_btn.add_css_class("flat");
    config_btn.add_css_class("config-path-btn");

    let btn_content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .hexpand(true)
        .build();

    let path_icon = gtk::Image::builder()
        .icon_name("folder-open-symbolic")
        .valign(gtk::Align::Center)
        .build();
    path_icon.add_css_class("dim-label");

    let config_label = gtk::Label::builder()
        .label(&config_path_str)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .wrap(true)
        .hexpand(true)
        .build();
    config_label.add_css_class("dim-label");
    config_label.add_css_class("caption");
    config_label.add_css_class("monospace");

    btn_content.append(&path_icon);
    btn_content.append(&config_label);
    config_btn.set_child(Some(&btn_content));

    {
        let config_path = config_path.clone();
        config_btn.connect_clicked(move |_| {
            open_in_file_explorer(&config_path);
        });
    }

    let copy_btn = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy path to clipboard")
        .valign(gtk::Align::Center)
        .build();
    copy_btn.add_css_class("flat");

    {
        let path_str = config_path_str.clone();
        let copy_btn_clone = copy_btn.clone();
        copy_btn.connect_clicked(move |_| {
            if let Some(disp) = gtk::gdk::Display::default() {
                disp.clipboard().set_text(&path_str);
            }
            copy_btn_clone.set_icon_name("object-select-symbolic");
            let btn_reset = copy_btn_clone.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                btn_reset.set_icon_name("edit-copy-symbolic");
            });
        });
    }

    config_row.append(&config_btn);
    config_row.append(&copy_btn);
    inner.append(&config_row);

    scroller.set_child(Some(&inner));
    sidebar.append(&scroller);

    let sync_sidebar: Rc<dyn Fn()> = {
        let config = config.clone();
        let is_syncing = is_syncing.clone();
        let dark_mode_switch = dark_mode_switch.clone();
        let light_preset_row = light_preset_row.clone();
        let dark_preset_row = dark_preset_row.clone();
        let smooth_scroll_switch = smooth_scroll_switch.clone();
        let startup_row = startup_row.clone();
        let theme_toggle_group = theme_toggle_group.clone();
        let font_size_scale = font_size_scale.clone();
        let ui_font_scale = ui_font_scale.clone();
        let toc_font_scale = toc_font_scale.clone();
        let theme_names: Vec<String> = theme_buttons.iter().map(|(n, _)| n.to_string()).collect();

        Rc::new(move || {
            is_syncing.set(true);
            let cfg = config.borrow();
            dark_mode_switch.set_active(cfg.is_dark());
            light_preset_row.set_selected(match cfg.light_theme.as_str() {
                "sepia" => 1,
                _ => 0,
            });
            dark_preset_row.set_selected(match cfg.dark_theme.as_str() {
                "charcoal" => 1,
                "oled" => 2,
                _ => 0,
            });
            smooth_scroll_switch.set_active(cfg.smooth_scrolling);
            startup_row.set_selected(match cfg.startup_behavior {
                crate::config::StartupBehavior::BlankPage => 0,
                crate::config::StartupBehavior::RecentPage => 1,
            });
            if let Some(ref scale) = font_size_scale {
                scale.set_value(cfg.font_size);
            }
            if let Some(ref scale) = ui_font_scale {
                scale.set_value(cfg.ui_font_size);
            }
            if let Some(ref scale) = toc_font_scale {
                scale.set_value(cfg.toc_font_size);
            }
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

/// Disable mouse wheel / touchpad scrolling on a gtk::Scale so it doesn't accidentally
/// change slider values when scrolling the settings panel, while forwarding the scroll
/// delta to the parent ScrolledWindow so the drawer scrolls smoothly.
pub fn disable_scroll_on_scale(scale: &gtk::Scale) {
    let scroll_controller = gtk::EventControllerScroll::new(
        gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::KINETIC,
    );
    scroll_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    scroll_controller.connect_scroll(move |controller, _dx, dy| {
        // Forward scroll delta to parent ScrolledWindow if present
        if let Some(widget) = controller.widget() {
            if let Some(ancestor) = widget.ancestor(gtk::ScrolledWindow::static_type()) {
                if let Some(scroller) = ancestor.downcast_ref::<gtk::ScrolledWindow>() {
                    let vadj = scroller.vadjustment();
                    let step = vadj.step_increment().max(35.0);
                    let page = vadj.page_size();
                    let upper = (vadj.upper() - page).max(0.0);
                    let new_val = (vadj.value() + dy * step).clamp(0.0, upper);
                    vadj.set_value(new_val);
                }
            }
        }
        // Stop propagation on Scale so it doesn't modify slider value
        glib::Propagation::Stop
    });
    scale.add_controller(scroll_controller);
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

    // Disable scroll wheel on scale so user doesn't accidentally change values
    disable_scroll_on_scale(&scale);

    let label_clone = label.clone();
    let label_text_owned = label_text.to_string();

    let on_change = Rc::new(on_change);

    scale.connect_value_changed(move |s| {
        let val = s.value();
        let text = if unit_owned.is_empty() {
            format!("{label_text_owned}: {val:.1}")
        } else {
            format!("{label_text_owned}: {val:.0}{unit_owned}")
        };
        label_clone.set_label(&text);
        on_change(val);
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
        .level(gtk::FontLevel::Family)
        .use_font(true)
        .use_size(false)
        .hexpand(true)
        .build();

    let mut initial_desc = gtk::pango::FontDescription::new();
    let clean_initial = initial_font.trim().trim_matches('"').trim_matches('\'').trim();
    initial_desc.set_family(clean_initial);
    font_btn.set_font_desc(&initial_desc);

    font_btn.connect_font_desc_notify(move |btn| {
        if let Some(desc) = btn.font_desc() {
            let family_opt = desc.family().map(|f| f.to_string()).or_else(|| {
                let s = desc.to_str();
                let f = s.split_whitespace().next().unwrap_or("Times New Roman");
                Some(f.to_string())
            });
            if let Some(family) = family_opt {
                let clean_family = family.trim().trim_matches('"').trim_matches('\'').trim().to_string();
                if !clean_family.is_empty() {
                    on_change(clean_family);
                }
            }
        }
    });

    container.append(&label);
    container.append(&font_btn);
    container
}

/// Open the directory containing a file in the default system file manager.
pub fn open_in_file_explorer(path: &std::path::Path) {
    let parent_dir = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };

    // Ensure parent directory exists so file manager doesn't report an error
    let _ = std::fs::create_dir_all(parent_dir);

    // Primary GTK4 method: open containing folder and highlight the file
    let gfile = gtk::gio::File::for_path(path);
    let launcher = gtk::FileLauncher::new(Some(&gfile));
    let path_buf = path.to_path_buf();
    launcher.open_containing_folder(
        None::<&gtk::Window>,
        None::<&gtk::gio::Cancellable>,
        move |res| {
            if res.is_err() {
                // Fallback 1: launch folder directly via GTK FileLauncher
                let parent = if path_buf.is_file() {
                    path_buf.parent().unwrap_or(&path_buf)
                } else {
                    &path_buf
                };
                let dir_gfile = gtk::gio::File::for_path(parent);
                let dir_launcher = gtk::FileLauncher::new(Some(&dir_gfile));
                let parent_buf = parent.to_path_buf();
                dir_launcher.launch(
                    None::<&gtk::Window>,
                    None::<&gtk::gio::Cancellable>,
                    move |dir_res| {
                        if dir_res.is_err() {
                            // Fallback 2: launch via xdg-open command
                            let _ = std::process::Command::new("xdg-open")
                                .arg(&parent_buf)
                                .spawn();
                        }
                    },
                );
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    #[test]
    fn test_theme_preset_combo_mapping() {
        let light_to_idx = |theme: &str| -> u32 {
            match theme {
                "sepia" => 1,
                _ => 0,
            }
        };

        let idx_to_light = |idx: u32| -> &'static str {
            match idx {
                1 => "sepia",
                _ => "light",
            }
        };

        assert_eq!(light_to_idx("light"), 0);
        assert_eq!(light_to_idx("sepia"), 1);
        assert_eq!(idx_to_light(0), "light");
        assert_eq!(idx_to_light(1), "sepia");

        let dark_to_idx = |theme: &str| -> u32 {
            match theme {
                "charcoal" => 1,
                "oled" => 2,
                _ => 0,
            }
        };

        let idx_to_dark = |idx: u32| -> &'static str {
            match idx {
                1 => "charcoal",
                2 => "oled",
                _ => "dark",
            }
        };

        assert_eq!(dark_to_idx("dark"), 0);
        assert_eq!(dark_to_idx("charcoal"), 1);
        assert_eq!(dark_to_idx("oled"), 2);
        assert_eq!(idx_to_dark(0), "dark");
        assert_eq!(idx_to_dark(1), "charcoal");
        assert_eq!(idx_to_dark(2), "oled");
    }

    #[test]
    fn test_config_path_resolution() {
        let path = Config::config_path();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("folio"));
        assert!(path_str.ends_with("config.toml"));
    }
}
