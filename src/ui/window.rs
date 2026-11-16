use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;

use crate::config::Config;
use crate::fetcher::ArticleData;
use crate::storage::Storage;
use crate::theme;
use crate::ui::settings_drawer;

/// Messages sent from the background tokio thread to the GTK main thread.
pub enum FetchMessage {
    Loading,
    Done(Box<ArticleData>),
    Error(String),
}

/// Build and present the main application window.
pub fn build_window(app: &adw::Application, initial_url: Option<String>) {
    // ── Shared State ──
    let config = Rc::new(RefCell::new(Config::load()));
    let storage = Rc::new(RefCell::new(
        Storage::open().ok()
    ));
    let css_provider = gtk::CssProvider::new();
    let auto_scrolling = Rc::new(Cell::new(false));
    let auto_scroll_tick_id: Rc<RefCell<Option<gtk::TickCallbackId>>> = Rc::new(RefCell::new(None));
    let current_url: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let zen_mode = Rc::new(Cell::new(false));

    // ── CSS ──
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    apply_css(&css_provider, &config.borrow());

    // ── Channel for async fetch results ──
    let (fetch_tx, fetch_rx) = async_channel::unbounded::<FetchMessage>();

    // ── Window ──
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Folio")
        .default_width(900)
        .default_height(700)
        .build();

    // ── Header Bar ──
    let header = adw::HeaderBar::new();

    // URL entry
    let url_entry = gtk::Entry::builder()
        .placeholder_text("Paste a URL and press Enter…")
        .hexpand(true)
        .build();

    // Dark / Light mode toggle button
    let dark_mode_btn = gtk::Button::builder()
        .icon_name(if config.borrow().is_dark() {
            "display-brightness-symbolic"
        } else {
            "weather-clear-night-symbolic"
        })
        .tooltip_text("Toggle dark/light mode (t)")
        .build();

    // Library button
    let library_btn = gtk::Button::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text("Saved articles")
        .build();

    // Settings button (Aa)
    let settings_btn = gtk::ToggleButton::builder()
        .label("Aa")
        .tooltip_text("Reading & Page Settings (Ctrl+,)")
        .build();

    header.set_title_widget(Some(&url_entry));
    header.pack_end(&settings_btn);
    header.pack_end(&dark_mode_btn);
    header.pack_end(&library_btn);

    // ── Reading Progress Bar ──
    let progress_bar = gtk::ProgressBar::new();
    progress_bar.add_css_class("reading-progress");
    progress_bar.set_fraction(0.0);
    progress_bar.set_visible(config.borrow().show_progress_bar);

    // ── Article Display Area ──
    let article_title_label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .selectable(true)
        .build();
    article_title_label.add_css_class("article-title");

    let article_byline_label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    article_byline_label.add_css_class("article-byline");

    let article_time_label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .build();
    article_time_label.add_css_class("article-reading-time");

    let article_body_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .halign(gtk::Align::Fill)
        .build();
    article_body_box.add_css_class("article-body");

    let article_column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .halign(gtk::Align::Center)
        .build();
    article_column.add_css_class("max-width-clamp");
    article_column.set_size_request(config.borrow().max_width as i32, -1);
    article_column.append(&article_title_label);
    article_column.append(&article_byline_label);
    article_column.append(&article_time_label);
    article_column.append(&article_body_box);

    let article_wrapper = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Start)
        .hexpand(true)
        .vexpand(true)
        .build();
    article_wrapper.add_css_class("article-content");
    article_wrapper.append(&article_column);

    let scrolled_window = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&article_wrapper)
        .build();
    scrolled_window.add_css_class("article-viewport");

    // ── Loading Spinner ──
    let spinner = gtk::Spinner::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .width_request(32)
        .height_request(32)
        .visible(false)
        .build();
    spinner.add_css_class("loading-spinner");

    // ── Empty State ──
    let empty_icon = gtk::Image::builder()
        .icon_name("document-viewer-symbolic")
        .pixel_size(72)
        .halign(gtk::Align::Center)
        .build();
    empty_icon.add_css_class("empty-state-icon");

    let empty_title = gtk::Label::builder()
        .label("Welcome to Folio")
        .halign(gtk::Align::Center)
        .build();
    empty_title.add_css_class("empty-state-title");

    let empty_subtitle = gtk::Label::builder()
        .label("Paste a URL above and press Enter to start reading")
        .halign(gtk::Align::Center)
        .wrap(true)
        .build();
    empty_subtitle.add_css_class("empty-state-subtitle");

    let empty_state = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .hexpand(true)
        .build();
    empty_state.add_css_class("empty-state");
    empty_state.append(&empty_icon);
    empty_state.append(&empty_title);
    empty_state.append(&empty_subtitle);

    // ── Content Stack (empty state / spinner / article) ──
    let content_stack = gtk::Stack::new();
    content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    content_stack.set_transition_duration(200);
    content_stack.add_named(&empty_state, Some("empty"));
    content_stack.add_named(&spinner, Some("loading"));
    content_stack.add_named(&scrolled_window, Some("article"));
    content_stack.set_visible_child_name("empty");

    // ── Main Content Box (header + progress + stack) ──
    let main_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    main_box.add_css_class("main-box");
    main_box.append(&header);
    main_box.append(&progress_bar);
    main_box.append(&content_stack);

    // ── Settings Sidebar & Config Sync ──
    let split_view_holder: Rc<RefCell<Option<adw::OverlaySplitView>>> = Rc::new(RefCell::new(None));
    let sync_sidebar_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    let last_css_key = Rc::new(RefCell::new(String::new()));
    let last_max_width = Rc::new(Cell::new(config.borrow().max_width));
    let last_progress_visible = Rc::new(Cell::new(config.borrow().show_progress_bar));
    let last_is_dark = Rc::new(Cell::new(config.borrow().is_dark()));
    let last_justified = Rc::new(Cell::new(config.borrow().justified));

    let on_config_changed: Rc<dyn Fn()> = {
        let css_provider = css_provider.clone();
        let config = config.clone();
        let progress_bar = progress_bar.clone();
        let article_column = article_column.clone();
        let article_body_box = article_body_box.clone();
        let dark_mode_btn = dark_mode_btn.clone();
        let sync_sidebar_holder = sync_sidebar_holder.clone();
        Rc::new(move || {
            let cfg = config.borrow();

            // 1. Only re-parse and re-apply CSS if styling properties changed
            let css_key = format!(
                "{}:{}:{:.1}:{:.1}:{:.1}:{:.1}:{}:{}:{}",
                cfg.theme,
                cfg.font_family,
                cfg.font_size,
                cfg.line_height,
                cfg.letter_spacing,
                cfg.margin,
                cfg.custom_bg,
                cfg.custom_fg,
                cfg.custom_accent,
            );
            if *last_css_key.borrow() != css_key {
                *last_css_key.borrow_mut() = css_key;
                apply_css(&css_provider, &cfg);
            }

            // 2. Only resize article column if max_width changed (instant layout without CSS reload)
            if (cfg.max_width - last_max_width.get()).abs() > f64::EPSILON {
                last_max_width.set(cfg.max_width);
                article_column.set_size_request(cfg.max_width as i32, -1);
            }

            // 3. Progress bar visibility
            if cfg.show_progress_bar != last_progress_visible.get() {
                last_progress_visible.set(cfg.show_progress_bar);
                progress_bar.set_visible(cfg.show_progress_bar);
            }

            // 4. Update dark mode button icon
            let is_dark = cfg.is_dark();
            if is_dark != last_is_dark.get() {
                last_is_dark.set(is_dark);
                dark_mode_btn.set_icon_name(if is_dark {
                    "display-brightness-symbolic"
                } else {
                    "weather-clear-night-symbolic"
                });
            }

            // 5. Update text alignment on existing paragraph labels ONLY when justified setting changed
            if cfg.justified != last_justified.get() {
                last_justified.set(cfg.justified);
                let is_justified = cfg.justified;
                let justify = if is_justified { gtk::Justification::Fill } else { gtk::Justification::Left };
                let halign = if is_justified { gtk::Align::Fill } else { gtk::Align::Start };

                let mut child_opt = article_body_box.first_child();
                while let Some(child) = child_opt {
                    if child.has_css_class("article-paragraph") {
                        if let Some(lbl) = child.downcast_ref::<gtk::Label>() {
                            lbl.set_justify(justify);
                            lbl.set_halign(halign);
                            lbl.set_xalign(0.0);
                        }
                    }
                    child_opt = child.next_sibling();
                }
            }

            // 6. Sync sidebar controls
            if let Some(ref sync_fn) = *sync_sidebar_holder.borrow() {
                sync_fn();
            }
        })
    };

    // Dark Mode Toggle Button (headerbar)
    {
        let config = config.clone();
        let on_config_changed = on_config_changed.clone();
        dark_mode_btn.connect_clicked(move |_| {
            config.borrow_mut().toggle_dark_mode();
            config.borrow().save();
            on_config_changed();
        });
    }

    let on_close_sidebar: Rc<dyn Fn()> = {
        let split_view_holder = split_view_holder.clone();
        Rc::new(move || {
            if let Some(ref sv) = *split_view_holder.borrow() {
                sv.set_show_sidebar(false);
            }
        })
    };

    let (sidebar, sync_sidebar) = settings_drawer::build_settings_sidebar(
        config.clone(),
        on_config_changed.clone(),
        on_close_sidebar,
    );
    *sync_sidebar_holder.borrow_mut() = Some(sync_sidebar);

    // ── OverlaySplitView ──
    let split_view = adw::OverlaySplitView::builder()
        .content(&main_box)
        .sidebar(&sidebar)
        .show_sidebar(false)
        .sidebar_position(gtk::PackType::End)
        .collapsed(true)
        .build();

    *split_view_holder.borrow_mut() = Some(split_view.clone());
    window.set_content(Some(&split_view));

    // ── Settings Button Toggle (Aa) ──
    {
        let sv = split_view.clone();
        settings_btn.connect_toggled(move |btn| {
            let active = btn.is_active();
            if sv.shows_sidebar() != active {
                sv.set_show_sidebar(active);
            }
        });
    }

    // Sync settings_btn when sidebar visibility changes externally (e.g. close button or Ctrl+,)
    {
        let settings_btn = settings_btn.clone();
        split_view.connect_show_sidebar_notify(move |sv| {
            let shows = sv.shows_sidebar();
            if settings_btn.is_active() != shows {
                settings_btn.set_active(shows);
            }
        });
    }

    // ── Library Popover ──
    {
        let storage = storage.clone();
        let url_entry = url_entry.clone();
        let fetch_tx = fetch_tx.clone();
        let library_btn_ref = library_btn.clone();
        let window_ref = window.clone();
        library_btn.connect_clicked(move |_| {
            show_library_dialog(
                &window_ref,
                &storage,
                &url_entry,
                &fetch_tx,
            );
            let _ = &library_btn_ref;
        });
    }

    // ── URL Entry Activation ──
    {
        let fetch_tx = fetch_tx.clone();
        let storage = storage.clone();
        url_entry.connect_activate(move |entry| {
            let raw = entry.text().to_string().trim().to_string();
            if raw.is_empty() {
                return;
            }
            let url = if raw.starts_with("http://") || raw.starts_with("https://") {
                raw
            } else {
                format!("https://{raw}")
            };
            entry.set_text(&url);
            trigger_fetch(&url, &fetch_tx, &storage);
        });
    }

    // ── Scroll Progress Tracking ──
    {
        let progress_bar = progress_bar.clone();
        let storage = storage.clone();
        let current_url = current_url.clone();
        let vadj = scrolled_window.vadjustment();
        vadj.connect_value_changed(move |adj| {
            let upper = adj.upper() - adj.page_size();
            if upper > 0.0 {
                let frac = adj.value() / upper;
                progress_bar.set_fraction(frac.clamp(0.0, 1.0));

                // Persist scroll position
                let url = current_url.borrow().clone();
                if !url.is_empty() {
                    if let Some(ref st) = *storage.borrow() {
                        let _ = st.update_scroll_position(&url, adj.value());
                    }
                }
            }
        });
    }

    // ── Receive Fetch Results ──
    {
        let content_stack = content_stack.clone();
        let spinner = spinner.clone();
        let article_title_label = article_title_label.clone();
        let article_byline_label = article_byline_label.clone();
        let article_time_label = article_time_label.clone();
        let article_body_box = article_body_box.clone();
        let storage = storage.clone();
        let current_url = current_url.clone();
        let scrolled_window = scrolled_window.clone();
        let config = config.clone();
        glib::spawn_future_local(async move {
            while let Ok(msg) = fetch_rx.recv().await {
                match msg {
                    FetchMessage::Loading => {
                        spinner.set_visible(true);
                        spinner.set_spinning(true);
                        content_stack.set_visible_child_name("loading");
                    }
                    FetchMessage::Done(article) => {
                        spinner.set_spinning(false);
                        spinner.set_visible(false);

                        article_title_label.set_text(&article.title);

                        if article.byline.is_empty() {
                            article_byline_label.set_visible(false);
                        } else {
                            article_byline_label.set_text(&article.byline);
                            article_byline_label.set_visible(true);
                        }

                        let time_str = format!("{} min read", article.reading_time_mins);
                        article_time_label.set_text(&time_str);

                        // Render rich article content
                        render_article_content(
                            &article_body_box,
                            &article.html_content,
                            &article.text_content,
                            config.borrow().justified,
                        );

                        content_stack.set_visible_child_name("article");

                        // Save to database
                        if let Some(ref st) = *storage.borrow() {
                            let _ = st.save_article(
                                &article.url,
                                &article.title,
                                &article.byline,
                                &article.html_content,
                                &article.text_content,
                                article.reading_time_mins,
                            );
                            let _ = st.record_history(&article.url, &article.title);
                        }

                        *current_url.borrow_mut() = article.url.clone();

                        // Restore scroll position
                        if let Some(ref st) = *storage.borrow() {
                            if let Ok(pos) = st.get_scroll_position(&article.url) {
                                if pos > 0.0 {
                                    let vadj = scrolled_window.vadjustment();
                                    glib::idle_add_local_once(move || {
                                        vadj.set_value(pos);
                                    });
                                }
                            }
                        }
                    }
                    FetchMessage::Error(err) => {
                        spinner.set_spinning(false);
                        spinner.set_visible(false);
                        article_title_label.set_text("Unable to Load Article");
                        article_byline_label.set_visible(false);
                        article_time_label.set_text("");
                        render_article_content(&article_body_box, "", &err, false);
                        content_stack.set_visible_child_name("article");
                    }
                }
            }
        });
    }

    // ── Keyboard Shortcuts & Smooth Scrolling ──
    {
        let key_controller = gtk::EventControllerKey::new();
        let url_entry = url_entry.clone();
        let split_view = split_view.clone();
        let scrolled_window = scrolled_window.clone();
        let config = config.clone();
        let on_config_changed = on_config_changed.clone();
        let auto_scrolling = auto_scrolling.clone();
        let auto_scroll_tick_id = auto_scroll_tick_id.clone();
        let window_clone = window.clone();
        let zen_mode = zen_mode.clone();
        let header = header.clone();

        let smooth_scroll_target: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(None));
        let smooth_scroll_tick_id: Rc<RefCell<Option<gtk::TickCallbackId>>> = Rc::new(RefCell::new(None));

        let smooth_scroll = {
            let scrolled_window = scrolled_window.clone();
            let target_cell = smooth_scroll_target.clone();
            let tick_id_cell = smooth_scroll_tick_id.clone();
            Rc::new(move |delta: f64| {
                let vadj = scrolled_window.vadjustment();
                let page = vadj.page_size();
                let upper = (vadj.upper() - page).max(0.0);
                let current = vadj.value();
                let base = target_cell.get().unwrap_or(current);
                let new_target = (base + delta).clamp(0.0, upper);
                target_cell.set(Some(new_target));

                if tick_id_cell.borrow().is_none() {
                    let tc = target_cell.clone();
                    let tic = tick_id_cell.clone();
                    let vadj_clone = vadj.clone();
                    let tick_id = scrolled_window.add_tick_callback(move |_widget, _frame_clock| {
                        let cur = vadj_clone.value();
                        if let Some(target) = tc.get() {
                            let diff = target - cur;
                            if diff.abs() < 0.8 {
                                vadj_clone.set_value(target);
                                tc.set(None);
                                *tic.borrow_mut() = None;
                                return glib::ControlFlow::Break;
                            }
                            vadj_clone.set_value(cur + diff * 0.22);
                            glib::ControlFlow::Continue
                        } else {
                            *tic.borrow_mut() = None;
                            glib::ControlFlow::Break
                        }
                    });
                    *tick_id_cell.borrow_mut() = Some(tick_id);
                }
            })
        };

        key_controller.connect_key_pressed(move |_, keyval, _keycode, modifier| {
            let ctrl = modifier.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = modifier.contains(gdk::ModifierType::SHIFT_MASK);
            let url_focused = url_entry.has_focus();

            match keyval {
                // Ctrl+L: Focus URL bar
                v if v == gdk::Key::l && ctrl => {
                    url_entry.grab_focus();
                    url_entry.select_region(0, -1);
                    return glib::Propagation::Stop;
                }
                // Ctrl+,: Toggle settings
                v if v == gdk::Key::comma && ctrl => {
                    split_view.set_show_sidebar(!split_view.shows_sidebar());
                    return glib::Propagation::Stop;
                }
                // F11: Zen mode
                v if v == gdk::Key::F11 => {
                    let is_zen = !zen_mode.get();
                    zen_mode.set(is_zen);
                    if is_zen {
                        window_clone.fullscreen();
                        header.set_visible(false);
                    } else {
                        window_clone.unfullscreen();
                        header.set_visible(true);
                    }
                    return glib::Propagation::Stop;
                }
                _ => {}
            }

            // Keys below only work when URL entry is NOT focused
            if url_focused {
                return glib::Propagation::Proceed;
            }

            match keyval {
                // j: Smooth scroll down
                v if v == gdk::Key::j => {
                    smooth_scroll(80.0);
                    glib::Propagation::Stop
                }
                // k: Smooth scroll up
                v if v == gdk::Key::k => {
                    smooth_scroll(-80.0);
                    glib::Propagation::Stop
                }
                // Space: Browser-like scroll down (Shift+Space: scroll up)
                v if v == gdk::Key::space => {
                    let page = (scrolled_window.vadjustment().page_size() * 0.85).max(180.0);
                    if shift {
                        smooth_scroll(-page);
                    } else {
                        smooth_scroll(page);
                    }
                    glib::Propagation::Stop
                }
                // PageDown: Smooth page down
                v if v == gdk::Key::Page_Down => {
                    let page = (scrolled_window.vadjustment().page_size() * 0.85).max(180.0);
                    smooth_scroll(page);
                    glib::Propagation::Stop
                }
                // PageUp: Smooth page up
                v if v == gdk::Key::Page_Up => {
                    let page = (scrolled_window.vadjustment().page_size() * 0.85).max(180.0);
                    smooth_scroll(-page);
                    glib::Propagation::Stop
                }
                // s: Toggle auto-scroll
                v if v == gdk::Key::s => {
                    if auto_scrolling.get() {
                        // Stop auto-scroll
                        auto_scrolling.set(false);
                        if let Some(tid) = auto_scroll_tick_id.borrow_mut().take() {
                            tid.remove();
                        }
                    } else {
                        // Start auto-scroll
                        auto_scrolling.set(true);
                        let vadj = scrolled_window.vadjustment();
                        let speed = config.borrow().auto_scroll_speed;
                        let as_flag = auto_scrolling.clone();
                        let tid = scrolled_window.add_tick_callback(move |_widget, frame_clock| {
                            if !as_flag.get() {
                                return glib::ControlFlow::Break;
                            }
                            let fps = frame_clock.fps();
                            let increment = if fps > 0.0 { speed / fps } else { speed / 60.0 };
                            vadj.set_value(vadj.value() + increment);
                            glib::ControlFlow::Continue
                        });
                        *auto_scroll_tick_id.borrow_mut() = Some(tid);
                    }
                    glib::Propagation::Stop
                }
                // t: Toggle dark/light mode
                v if v == gdk::Key::t => {
                    {
                        let mut cfg = config.borrow_mut();
                        cfg.toggle_dark_mode();
                        cfg.save();
                    }
                    on_config_changed();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        window.add_controller(key_controller);
    }

    // ── Clipboard Auto-Detect ──
    if config.borrow().auto_detect_clipboard {
        let clipboard = window.clipboard();
        let fetch_tx = fetch_tx.clone();
        let url_entry = url_entry.clone();
        let window_ref = window.clone();
        let storage_clip = storage.clone();
        glib::idle_add_local_once(move || {
            clipboard.read_text_async(None::<&gtk::gio::Cancellable>, move |result| {
                if let Ok(Some(text)) = result {
                    let trimmed = text.trim().to_string();
                    if looks_like_url(&trimmed) {
                        let dialog = adw::AlertDialog::builder()
                            .heading("Open Clipboard URL?")
                            .body(&trimmed)
                            .build();
                        dialog.add_response("cancel", "Cancel");
                        dialog.add_response("open", "Open");
                        dialog.set_default_response(Some("open"));
                        dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);

                        let tx = fetch_tx.clone();
                        let entry = url_entry.clone();
                        let storage_clone = storage_clip.clone();
                        let url = trimmed.clone();
                        dialog.connect_response(None, move |_, response| {
                            if response == "open" {
                                entry.set_text(&url);
                                trigger_fetch(&url, &tx, &storage_clone);
                            }
                        });
                        dialog.present(Some(&window_ref));
                    }
                }
            });
        });
    }

    // ── Open initial URL if provided ──
    if let Some(url) = initial_url {
        url_entry.set_text(&url);
        trigger_fetch(&url, &fetch_tx, &storage);
    }

    window.present();
}

/// Apply CSS from the current config to the display.
fn apply_css(provider: &gtk::CssProvider, config: &Config) {
    let css = theme::generate_css(config);
    provider.load_from_string(&css);

    // Also set the libadwaita color scheme
    let style_manager = adw::StyleManager::default();
    match config.theme.as_str() {
        "dark" | "oled" => style_manager.set_color_scheme(adw::ColorScheme::ForceDark),
        "light" | "sepia" => style_manager.set_color_scheme(adw::ColorScheme::ForceLight),
        _ => style_manager.set_color_scheme(adw::ColorScheme::Default),
    }
}

/// Trigger an async fetch on the tokio runtime, checking offline cache first.
fn trigger_fetch(
    url: &str,
    tx: &async_channel::Sender<FetchMessage>,
    storage: &Rc<RefCell<Option<Storage>>>,
) {
    let url_str = url.to_string();
    let mut has_cached = false;

    // Check offline cache first for instant display (only if substantial content exists)
    if let Some(ref st) = *storage.borrow() {
        if let Ok(Some(cached)) = st.get_article(&url_str) {
            if cached.text_content.split_whitespace().count() >= 50 {
                has_cached = true;
                let _ = tx.send_blocking(FetchMessage::Done(Box::new(ArticleData {
                    url: cached.url,
                    title: cached.title,
                    byline: cached.byline,
                    html_content: cached.html_content,
                    text_content: cached.text_content,
                    reading_time_mins: cached.reading_time_mins,
                })));
            }
        }
    }

    if !has_cached {
        let _ = tx.send_blocking(FetchMessage::Loading);
    }

    let tx = tx.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");
        rt.block_on(async {
            match crate::fetcher::fetch_article(&url_str).await {
                Ok(article) => {
                    let _ = tx.send(FetchMessage::Done(Box::new(article))).await;
                }
                Err(e) => {
                    if !has_cached {
                        let _ = tx.send(FetchMessage::Error(e)).await;
                    }
                }
            }
        });
    });
}

/// Check if a string looks like a URL.
fn looks_like_url(s: &str) -> bool {
    if s.starts_with("http://") || s.starts_with("https://") {
        return url::Url::parse(s).is_ok();
    }
    false
}

/// Show a dialog listing saved articles from the library.
fn show_library_dialog(
    window: &adw::ApplicationWindow,
    storage: &Rc<RefCell<Option<Storage>>>,
    url_entry: &gtk::Entry,
    fetch_tx: &async_channel::Sender<FetchMessage>,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Saved Articles")
        .build();
    dialog.add_response("close", "Close");

    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list_box.add_css_class("library-list");
    list_box.add_css_class("boxed-list");

    if let Some(ref st) = *storage.borrow() {
        match st.list_articles() {
            Ok(articles) => {
                if articles.is_empty() {
                    let empty = gtk::Label::builder()
                        .label("No saved articles yet.\nArticles you read will appear here.")
                        .halign(gtk::Align::Center)
                        .margin_top(24)
                        .margin_bottom(24)
                        .build();
                    empty.add_css_class("dim-label");
                    list_box.append(&empty);
                } else {
                    for article in articles.iter().take(50) {
                        let raw_date = article.last_read_at.as_deref().unwrap_or(&article.saved_at);
                        let date_str = if raw_date.len() >= 10 {
                            &raw_date[..10]
                        } else {
                            raw_date
                        };
                        let prefix = if article.last_read_at.is_some() { "Read" } else { "Saved" };
                        let progress_hint = if article.scroll_position > 0.0 { " • In progress" } else { "" };
                        let subtitle_text = format!("{} min{} • {} {} • {}", article.reading_time_mins, progress_hint, prefix, date_str, article.url);

                        let row = adw::ActionRow::builder()
                            .title(&*glib::markup_escape_text(&article.title))
                            .subtitle(&*glib::markup_escape_text(&subtitle_text))
                            .activatable(true)
                            .build();

                        // Delete button
                        let del_btn = gtk::Button::builder()
                            .icon_name("user-trash-symbolic")
                            .has_frame(false)
                            .tooltip_text("Delete from library")
                            .valign(gtk::Align::Center)
                            .build();

                        let st_clone = storage.clone();
                        let list_clone = list_box.clone();
                        let row_clone = row.clone();
                        let article_id = article.id;
                        del_btn.connect_clicked(move |_| {
                            if let Some(ref st) = *st_clone.borrow() {
                                let _ = st.delete_article(article_id);
                            }
                            list_clone.remove(&row_clone);
                        });
                        row.add_suffix(&del_btn);

                        let url = article.url.clone();
                        let entry = url_entry.clone();
                        let tx = fetch_tx.clone();
                        let dlg = dialog.clone();
                        let cached_data = Box::new(ArticleData {
                            url: article.url.clone(),
                            title: article.title.clone(),
                            byline: article.byline.clone(),
                            html_content: article.html_content.clone(),
                            text_content: article.text_content.clone(),
                            reading_time_mins: article.reading_time_mins,
                        });

                        row.connect_activated(move |_| {
                            entry.set_text(&url);
                            let _ = tx.send_blocking(FetchMessage::Done(cached_data.clone()));
                            dlg.close();
                        });

                        list_box.append(&row);
                    }
                }
            }
            Err(e) => {
                let err_label = gtk::Label::builder()
                    .label(&format!("Error: {e}"))
                    .build();
                list_box.append(&err_label);
            }
        }
    } else {
        let err_label = gtk::Label::builder()
            .label("Database not available")
            .build();
        list_box.append(&err_label);
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(350)
        .min_content_width(450)
        .child(&list_box)
        .build();

    dialog.set_extra_child(Some(&scrolled));
    dialog.present(Some(window));
}

/// Sanitize basic HTML tags into Pango markup tags.
fn sanitize_to_pango(html: &str) -> String {
    html.replace("<strong>", "<b>")
        .replace("</strong>", "</b>")
        .replace("<em>", "<i>")
        .replace("</em>", "</i>")
        .replace("<code>", "<tt>")
        .replace("</code>", "</tt>")
        .replace("<del>", "<s>")
        .replace("</del>", "</s>")
        .replace("<ins>", "<u>")
        .replace("</ins>", "</u>")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
}

/// Render structured article blocks into a single continuous rich selectable text document.
/// Enables continuous drag selection across all paragraphs, Ctrl+A to select all, and instant 120 FPS live preview.
fn render_article_content(box_widget: &gtk::Box, html: &str, text: &str, justified: bool) {
    while let Some(child) = box_widget.first_child() {
        box_widget.remove(&child);
    }

    let p_halign = if justified { gtk::Align::Fill } else { gtk::Align::Start };
    let p_justify = if justified { gtk::Justification::Fill } else { gtk::Justification::Left };

    let mut full_markup = String::with_capacity(html.len() + 2048);

    if !html.is_empty() {
        let doc = dom_query::Document::from(html);
        let nodes = doc.select("p, h1, h2, h3, h4, h5, h6, blockquote, pre, li");
        for node in nodes.iter() {
            let tag = node
                .nodes()
                .first()
                .and_then(|n| n.node_name())
                .unwrap_or_default()
                .to_string()
                .to_lowercase();
            let text_val = node.text().to_string();
            let trimmed = text_val.trim();
            if trimmed.is_empty() {
                continue;
            }

            match tag.as_str() {
                "h1" | "h2" => {
                    let escaped = glib::markup_escape_text(trimmed);
                    full_markup.push_str(&format!("\n<span size=\"xx-large\" weight=\"800\">{}</span>\n\n", escaped));
                }
                "h3" | "h4" | "h5" | "h6" => {
                    let escaped = glib::markup_escape_text(trimmed);
                    full_markup.push_str(&format!("\n<span size=\"x-large\" weight=\"bold\">{}</span>\n\n", escaped));
                }
                "blockquote" => {
                    let escaped = glib::markup_escape_text(trimmed);
                    full_markup.push_str(&format!("\n<span style=\"italic\">“{}”</span>\n\n", escaped));
                }
                "pre" => {
                    let escaped = glib::markup_escape_text(trimmed);
                    full_markup.push_str(&format!("\n<tt>{}</tt>\n\n", escaped));
                }
                "li" => {
                    let escaped = glib::markup_escape_text(trimmed);
                    full_markup.push_str(&format!("  • {}\n", escaped));
                }
                "p" => {
                    let inner_html = node.inner_html().to_string();
                    let pango = sanitize_to_pango(&inner_html);
                    full_markup.push_str(&pango);
                    full_markup.push_str("\n\n");
                }
                _ => {}
            }
        }
    }

    if full_markup.trim().is_empty() {
        for para in text.split("\n\n") {
            let trimmed = para.trim();
            if !trimmed.is_empty() {
                let escaped = glib::markup_escape_text(trimmed);
                full_markup.push_str(&escaped);
                full_markup.push_str("\n\n");
            }
        }
    }

    let label = gtk::Label::builder()
        .halign(p_halign)
        .justify(p_justify)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    label.add_css_class("article-body-text");
    label.add_css_class("article-paragraph");

    let trimmed_markup = full_markup.trim().to_string();
    if gtk::pango::parse_markup(&trimmed_markup, '\0').is_ok() {
        label.set_markup(&trimmed_markup);
    } else {
        label.set_text(text);
    }

    label.connect_activate_link(|_lbl, uri| {
        let _ = gtk::gio::AppInfo::launch_default_for_uri(uri, None::<&gtk::gio::AppLaunchContext>);
        glib::Propagation::Stop
    });

    box_widget.append(&label);
}
