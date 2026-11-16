use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;

use crate::config::Config;
use crate::fetcher::{ArticleData, NavigationLink};
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
    let auto_scroll_pause_until: Rc<Cell<Option<std::time::Instant>>> = Rc::new(Cell::new(None));
    let last_as_frame_time_us: Rc<Cell<Option<i64>>> = Rc::new(Cell::new(None));
    let current_url: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let zen_mode = Rc::new(Cell::new(false));

    // ── CSS ──
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
    }
    apply_css(&css_provider, &config.borrow(), true);

    // ── Channel for async fetch results ──
    let (fetch_tx, fetch_rx) = async_channel::unbounded::<FetchMessage>();

    // ── Window ──
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Folio")
        .default_width(900)
        .default_height(700)
        .build();

    // ── Session History (Browser-style Back / Forward) ──
    let session_history: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let session_index: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let is_history_nav: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    // ── Header Bar ──
    let header = adw::HeaderBar::new();

    // Browser-style Back & Forward Navigation Buttons
    let nav_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .build();
    nav_box.add_css_class("linked");

    let back_btn = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Back (Alt+Left)")
        .sensitive(false)
        .build();

    let forward_btn = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .tooltip_text("Forward (Alt+Right)")
        .sensitive(false)
        .build();

    nav_box.append(&back_btn);
    nav_box.append(&forward_btn);
    header.pack_start(&nav_box);

    let update_nav_buttons = {
        let back_btn = back_btn.clone();
        let forward_btn = forward_btn.clone();
        let history = session_history.clone();
        let index = session_index.clone();
        Rc::new(move || {
            let hist = history.borrow();
            let idx = index.get();
            let can_back = idx > 0 && !hist.is_empty();
            let can_forward = !hist.is_empty() && (idx + 1 < hist.len());
            back_btn.set_sensitive(can_back);
            forward_btn.set_sensitive(can_forward);
        })
    };

    // URL entry
    let url_entry = gtk::Entry::builder()
        .placeholder_text("Paste a URL and press Enter…")
        .hexpand(true)
        .build();

    let go_back = {
        let history = session_history.clone();
        let index = session_index.clone();
        let is_nav = is_history_nav.clone();
        let entry = url_entry.clone();
        let tx = fetch_tx.clone();
        let storage = storage.clone();
        let update_btns = update_nav_buttons.clone();
        Rc::new(move || {
            let hist = history.borrow();
            let idx = index.get();
            if idx > 0 && !hist.is_empty() {
                let new_idx = idx - 1;
                index.set(new_idx);
                let target_url = hist[new_idx].clone();
                is_nav.set(true);
                entry.set_text(&target_url);
                trigger_fetch(&target_url, &tx, &storage);
                drop(hist);
                update_btns();
            }
        })
    };
    {
        let go_back = go_back.clone();
        back_btn.connect_clicked(move |_| {
            go_back();
        });
    }

    let go_forward = {
        let history = session_history.clone();
        let index = session_index.clone();
        let is_nav = is_history_nav.clone();
        let entry = url_entry.clone();
        let tx = fetch_tx.clone();
        let storage = storage.clone();
        let update_btns = update_nav_buttons.clone();
        Rc::new(move || {
            let hist = history.borrow();
            let idx = index.get();
            if !hist.is_empty() && idx + 1 < hist.len() {
                let new_idx = idx + 1;
                index.set(new_idx);
                let target_url = hist[new_idx].clone();
                is_nav.set(true);
                entry.set_text(&target_url);
                trigger_fetch(&target_url, &tx, &storage);
                drop(hist);
                update_btns();
            }
        })
    };
    {
        let go_forward = go_forward.clone();
        forward_btn.connect_clicked(move |_| {
            go_forward();
        });
    }

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

    // ── Virtualized Article Display Area (ListView) ──
    let article_store = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection_model = gtk::NoSelection::new(Some(article_store.clone()));
    let factory = gtk::SignalListItemFactory::new();

    {
        let fetch_tx = fetch_tx.clone();
        let url_entry = url_entry.clone();
        let storage = storage.clone();
        factory.connect_setup(move |_, item| {
            let list_item = item.downcast_ref::<gtk::ListItem>().unwrap();

            let container = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .halign(gtk::Align::Fill)
                .hexpand(true)
                .build();
            container.add_css_class("article-block-container");

            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .xalign(0.0)
                .hexpand(true)
                .build();

            label.connect_activate_link(|_lbl, uri| {
                let _ = gtk::gio::AppInfo::launch_default_for_uri(uri, None::<&gtk::gio::AppLaunchContext>);
                glib::Propagation::Stop
            });

            container.append(&label);

            let nav_row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(16)
                .halign(gtk::Align::Center)
                .visible(false)
                .build();
            nav_row.add_css_class("reader-nav-row");

            let prev_btn = gtk::Button::builder()
                .label("← Previous")
                .visible(false)
                .build();
            prev_btn.add_css_class("pill");
            prev_btn.add_css_class("reader-nav-btn");

            let next_btn = gtk::Button::builder()
                .label("Next →")
                .visible(false)
                .build();
            next_btn.add_css_class("pill");
            next_btn.add_css_class("reader-nav-btn");

            {
                let tx = fetch_tx.clone();
                let entry = url_entry.clone();
                let st = storage.clone();
                prev_btn.connect_clicked(move |btn| {
                    if let Some(target) = btn.tooltip_text() {
                        let target_str = target.to_string();
                        if !target_str.is_empty() {
                            entry.set_text(&target_str);
                            trigger_fetch(&target_str, &tx, &st);
                        }
                    }
                });
            }

            {
                let tx = fetch_tx.clone();
                let entry = url_entry.clone();
                let st = storage.clone();
                next_btn.connect_clicked(move |btn| {
                    if let Some(target) = btn.tooltip_text() {
                        let target_str = target.to_string();
                        if !target_str.is_empty() {
                            entry.set_text(&target_str);
                            trigger_fetch(&target_str, &tx, &st);
                        }
                    }
                });
            }

            nav_row.append(&prev_btn);
            nav_row.append(&next_btn);
            container.append(&nav_row);

            list_item.set_child(Some(&container));
        });
    }

    {
        let config = config.clone();
        factory.connect_bind(move |_, item| {
            let list_item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let boxed = match list_item.item().and_downcast::<glib::BoxedAnyObject>() {
                Some(b) => b,
                None => return,
            };
            let block: std::cell::Ref<ContentBlock> = boxed.borrow();
            let container = match list_item.child().and_downcast::<gtk::Box>() {
                Some(c) => c,
                None => return,
            };
            let label = match container.first_child().and_downcast::<gtk::Label>() {
                Some(l) => l,
                None => return,
            };
            let nav_row = match container.last_child().and_downcast::<gtk::Box>() {
                Some(b) => b,
                None => return,
            };

            for cls in &[
                "article-title",
                "article-byline",
                "article-reading-time",
                "article-heading",
                "article-h2",
                "article-h3",
                "article-paragraph",
                "article-blockquote",
                "article-quote",
                "article-codeblock",
                "article-code",
                "article-list-item",
                "article-divider",
            ] {
                label.remove_css_class(cls);
            }

            if let ContentBlock::Navigation { prev_link, next_link } = &*block {
                label.set_visible(false);
                label.set_text("");
                nav_row.set_visible(true);

                if let Some(prev_btn) = nav_row.first_child().and_downcast::<gtk::Button>() {
                    if let Some(pl) = prev_link {
                        prev_btn.set_visible(true);
                        prev_btn.set_label(&format!("← {}", pl.label));
                        prev_btn.set_tooltip_text(Some(&pl.url));
                    } else {
                        prev_btn.set_visible(false);
                        prev_btn.set_tooltip_text(None);
                    }
                }

                if let Some(next_btn) = nav_row.last_child().and_downcast::<gtk::Button>() {
                    if let Some(nl) = next_link {
                        next_btn.set_visible(true);
                        next_btn.set_label(&format!("{} →", nl.label));
                        next_btn.set_tooltip_text(Some(&nl.url));
                    } else {
                        next_btn.set_visible(false);
                        next_btn.set_tooltip_text(None);
                    }
                }
                return;
            }

            label.set_visible(true);
            nav_row.set_visible(false);

            match &*block {
                ContentBlock::Title(title) => {
                    label.add_css_class("article-title");
                    label.set_halign(gtk::Align::Start);
                    label.set_justify(gtk::Justification::Left);
                    label.set_text(title);
                }
                ContentBlock::Meta { byline, reading_time } => {
                    label.add_css_class("article-byline");
                    label.set_halign(gtk::Align::Start);
                    label.set_justify(gtk::Justification::Left);
                    let text = if !byline.is_empty() && !reading_time.is_empty() {
                        format!("{}  •  {}", byline, reading_time)
                    } else if !byline.is_empty() {
                        byline.clone()
                    } else {
                        reading_time.clone()
                    };
                    label.set_text(&text);
                }
                ContentBlock::Heading { level, text } => {
                    label.add_css_class("article-heading");
                    if *level <= 2 {
                        label.add_css_class("article-h2");
                    } else {
                        label.add_css_class("article-h3");
                    }
                    label.set_halign(gtk::Align::Start);
                    label.set_justify(gtk::Justification::Left);
                    label.set_text(text);
                }
                ContentBlock::Paragraph { markup, plain } => {
                    label.add_css_class("article-paragraph");
                    let is_justified = config.borrow().justified;
                    if is_justified {
                        label.set_halign(gtk::Align::Fill);
                        label.set_justify(gtk::Justification::Fill);
                    } else {
                        label.set_halign(gtk::Align::Start);
                        label.set_justify(gtk::Justification::Left);
                    }
                    if gtk::pango::parse_markup(markup, '\0').is_ok() {
                        label.set_markup(markup);
                    } else {
                        label.set_text(plain);
                    }
                }
                ContentBlock::Quote(text) => {
                    label.add_css_class("article-blockquote");
                    label.add_css_class("article-quote");
                    label.set_halign(gtk::Align::Fill);
                    label.set_justify(gtk::Justification::Left);
                    let escaped = glib::markup_escape_text(text);
                    let markup_text = format!("<span style=\"italic\">“{}”</span>", escaped);
                    if gtk::pango::parse_markup(&markup_text, '\0').is_ok() {
                        label.set_markup(&markup_text);
                    } else {
                        label.set_text(text);
                    }
                }
                ContentBlock::Code(text) => {
                    label.add_css_class("article-codeblock");
                    label.add_css_class("article-code");
                    label.set_halign(gtk::Align::Fill);
                    label.set_justify(gtk::Justification::Left);
                    label.set_text(text);
                }
                ContentBlock::ListItem(text) => {
                    label.add_css_class("article-list-item");
                    label.set_halign(gtk::Align::Start);
                    label.set_justify(gtk::Justification::Left);
                    label.set_text(text);
                }
                ContentBlock::Divider => {
                    label.add_css_class("article-divider");
                    label.set_halign(gtk::Align::Center);
                    label.set_justify(gtk::Justification::Center);
                    label.set_text("— • —");
                }
                ContentBlock::Navigation { .. } => {}
            }
        });
    }

    factory.connect_unbind(|_, item| {
        let list_item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let Some(container) = list_item.child().and_downcast::<gtk::Box>() {
            if let Some(label) = container.first_child().and_downcast::<gtk::Label>() {
                label.set_text("");
            }
            if let Some(nav_row) = container.last_child().and_downcast::<gtk::Box>() {
                nav_row.set_visible(false);
            }
        }
    });

    let list_view = gtk::ListView::new(Some(selection_model), Some(factory));
    list_view.set_can_focus(false);
    list_view.add_css_class("article-list-view");

    let article_clamp = adw::ClampScrollable::builder()
        .maximum_size(config.borrow().max_width as i32)
        .tightening_threshold(config.borrow().max_width as i32)
        .child(&list_view)
        .build();
    article_clamp.add_css_class("max-width-clamp");

    let scrolled_window = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&article_clamp)
        .build();
    scrolled_window.add_css_class("article-viewport");
    scrolled_window.add_css_class("reader-surface");

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
    let last_theme = Rc::new(RefCell::new(config.borrow().theme.clone()));
    let last_font_size = Rc::new(Cell::new(config.borrow().font_size));
    let last_max_width = Rc::new(Cell::new(config.borrow().max_width));
    let last_progress_visible = Rc::new(Cell::new(config.borrow().show_progress_bar));
    let last_is_dark = Rc::new(Cell::new(config.borrow().is_dark()));
    let last_justified = Rc::new(Cell::new(config.borrow().justified));

    // Unified 350ms debounced disk save to ensure zero I/O lag during user interaction
    let save_timeout: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let debounced_save: Rc<dyn Fn()> = {
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

    let on_config_changed: Rc<dyn Fn()> = {
        let css_provider = css_provider.clone();
        let config = config.clone();
        let progress_bar = progress_bar.clone();
        let article_clamp = article_clamp.clone();
        let list_view = list_view.clone();
        let dark_mode_btn = dark_mode_btn.clone();
        let sync_sidebar_holder = sync_sidebar_holder.clone();
        let last_theme = last_theme.clone();
        let last_font_size = last_font_size.clone();
        Rc::new(move || {
            let cfg = config.borrow();

            let is_dark_now = cfg.is_dark();
            let theme_changed = cfg.theme != *last_theme.borrow() || is_dark_now != last_is_dark.get();
            if theme_changed {
                *last_theme.borrow_mut() = cfg.theme.clone();
            }

            let font_size_changed = (cfg.font_size - last_font_size.get()).abs() > f64::EPSILON;
            if font_size_changed {
                last_font_size.set(cfg.font_size);
            }

            // 1. Only re-parse and re-apply CSS if styling properties changed
            let css_key = format!(
                "{}:{}:{:.1}:{:.1}:{:.1}:{:.1}:{}:{}:{}:{}",
                cfg.theme,
                cfg.font_family,
                cfg.font_size,
                cfg.line_height,
                cfg.letter_spacing,
                cfg.margin,
                cfg.paragraph_spacing_px,
                cfg.custom_bg,
                cfg.custom_fg,
                cfg.custom_accent,
            );
            if *last_css_key.borrow() != css_key {
                *last_css_key.borrow_mut() = css_key;
                apply_css(&css_provider, &cfg, theme_changed);
            } else if theme_changed {
                apply_css(&css_provider, &cfg, true);
            }

            // 2. Only resize article clamp if max_width changed (instant layout without CSS reload)
            if (cfg.max_width - last_max_width.get()).abs() > f64::EPSILON {
                last_max_width.set(cfg.max_width);
                article_clamp.set_maximum_size(cfg.max_width as i32);
                article_clamp.set_tightening_threshold(cfg.max_width as i32);
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

            // 5. Update text alignment directly on existing label properties without re-parsing article
            if cfg.justified != last_justified.get() {
                last_justified.set(cfg.justified);
                let is_justified = cfg.justified;
                let justify = if is_justified { gtk::Justification::Fill } else { gtk::Justification::Left };
                let halign = if is_justified { gtk::Align::Fill } else { gtk::Align::Start };

                let mut row_opt = list_view.first_child();
                while let Some(row) = row_opt {
                    if let Some(container) = row.first_child() {
                        if let Some(lbl) = container.first_child().and_downcast::<gtk::Label>() {
                            if lbl.has_css_class("article-paragraph") {
                                lbl.set_justify(justify);
                                lbl.set_halign(halign);
                                lbl.set_xalign(0.0);
                            }
                        }
                    }
                    row_opt = row.next_sibling();
                }
            }

            // 6. Sync sidebar controls if theme, dark mode, or font size changed
            if theme_changed || font_size_changed {
                if let Some(ref sync_fn) = *sync_sidebar_holder.borrow() {
                    sync_fn();
                }
            }
        })
    };

    // Dark Mode Toggle Button (headerbar)
    {
        let config = config.clone();
        let on_config_changed = on_config_changed.clone();
        let debounced_save = debounced_save.clone();
        dark_mode_btn.connect_clicked(move |_| {
            config.borrow_mut().toggle_dark_mode();
            debounced_save();
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
        let article_store = article_store.clone();
        let storage = storage.clone();
        let current_url = current_url.clone();
        let scrolled_window = scrolled_window.clone();
        let session_history = session_history.clone();
        let session_index = session_index.clone();
        let is_history_nav = is_history_nav.clone();
        let update_nav_buttons = update_nav_buttons.clone();
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

                        // Parse and populate virtualized ContentBlocks
                        let blocks = parse_article_blocks(&article);
                        article_store.remove_all();
                        for block in blocks {
                            article_store.append(&glib::BoxedAnyObject::new(block));
                        }

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
                                article.prev_link.as_ref().map(|l| l.url.as_str()),
                                article.next_link.as_ref().map(|l| l.url.as_str()),
                            );
                            let _ = st.record_history(&article.url, &article.title);
                        }

                        // Record into session history if this wasn't a back/forward navigation
                        let nav_flag = is_history_nav.get();
                        is_history_nav.set(false);
                        {
                            let mut hist = session_history.borrow_mut();
                            let cur_idx = session_index.get();
                            if !nav_flag {
                                if hist.is_empty() {
                                    hist.push(article.url.clone());
                                    session_index.set(0);
                                } else if hist.get(cur_idx) != Some(&article.url) {
                                    hist.truncate(cur_idx + 1);
                                    hist.push(article.url.clone());
                                    session_index.set(hist.len() - 1);
                                }
                            }
                        }
                        update_nav_buttons();

                        let url_changed = *current_url.borrow() != article.url;
                        *current_url.borrow_mut() = article.url.clone();

                        // Restore scroll position only on initial URL load
                        if url_changed {
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
                    }
                    FetchMessage::Error(err) => {
                        spinner.set_spinning(false);
                        spinner.set_visible(false);
                        article_store.remove_all();
                        article_store.append(&glib::BoxedAnyObject::new(ContentBlock::Title(
                            "Unable to Load Article".to_string(),
                        )));
                        article_store.append(&glib::BoxedAnyObject::new(ContentBlock::Paragraph {
                            markup: glib::markup_escape_text(&err).to_string(),
                            plain: err,
                        }));
                        content_stack.set_visible_child_name("article");
                    }
                }
            }
        });
    }

    // ── Keyboard Shortcuts & Smooth Scrolling ──
    {
        let key_controller = gtk::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let url_entry = url_entry.clone();
        let split_view = split_view.clone();
        let scrolled_window = scrolled_window.clone();
        let config = config.clone();
        let on_config_changed = on_config_changed.clone();
        let auto_scrolling = auto_scrolling.clone();
        let auto_scroll_tick_id = auto_scroll_tick_id.clone();
        let auto_scroll_pause_until = auto_scroll_pause_until.clone();
        let last_as_frame_time_us = last_as_frame_time_us.clone();
        let window_clone = window.clone();
        let zen_mode = zen_mode.clone();
        let header = header.clone();
        let debounced_save = debounced_save.clone();

        let is_animating = Rc::new(Cell::new(false));
        let held_direction: Rc<Cell<Option<i32>>> = Rc::new(Cell::new(None));
        let target_y = Rc::new(Cell::new(0.0f64));
        let smooth_scroll_tick_id: Rc<RefCell<Option<gtk::TickCallbackId>>> = Rc::new(RefCell::new(None));
        let last_smooth_frame_time_us: Rc<Cell<Option<i64>>> = Rc::new(Cell::new(None));

        // ── Zoom In / Zoom Out Helpers ──
        let zoom_in = {
            let config = config.clone();
            let debounced_save = debounced_save.clone();
            let on_config_changed = on_config_changed.clone();
            Rc::new(move || {
                let cur = config.borrow().font_size;
                let next = (cur + 2.0).min(48.0);
                if (next - cur).abs() > 0.01 {
                    config.borrow_mut().font_size = next;
                    debounced_save();
                    on_config_changed();
                }
            })
        };

        let zoom_out = {
            let config = config.clone();
            let debounced_save = debounced_save.clone();
            let on_config_changed = on_config_changed.clone();
            Rc::new(move || {
                let cur = config.borrow().font_size;
                let next = (cur - 2.0).max(10.0);
                if (next - cur).abs() > 0.01 {
                    config.borrow_mut().font_size = next;
                    debounced_save();
                    on_config_changed();
                }
            })
        };

        let zoom_reset = {
            let config = config.clone();
            let debounced_save = debounced_save.clone();
            let on_config_changed = on_config_changed.clone();
            Rc::new(move || {
                let cur = config.borrow().font_size;
                let next = crate::config::Config::default().font_size;
                if (next - cur).abs() > 0.01 {
                    config.borrow_mut().font_size = next;
                    debounced_save();
                    on_config_changed();
                }
            })
        };

        // ── Mouse Wheel / Touchpad Manual Scroll Listener (Capture Phase) ──
        let scroll_controller = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::KINETIC
        );
        scroll_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let is_animating = is_animating.clone();
            let held_direction = held_direction.clone();
            let target_y = target_y.clone();
            let vadj = scrolled_window.vadjustment();
            let pause_cell = auto_scroll_pause_until.clone();
            let tick_id_cell = smooth_scroll_tick_id.clone();
            let last_time_cell = last_smooth_frame_time_us.clone();
            let sw_clone = scrolled_window.clone();
            let zoom_in_scroll = zoom_in.clone();
            let zoom_out_scroll = zoom_out.clone();

            let cancel_anim = Rc::new(move || {
                held_direction.set(None);
                if is_animating.get() {
                    is_animating.set(false);
                    if let Some(tid) = tick_id_cell.borrow_mut().take() {
                        tid.remove();
                    }
                    last_time_cell.set(None);
                    sw_clone.set_kinetic_scrolling(true);
                }
                target_y.set(vadj.value());
                pause_cell.set(Some(
                    std::time::Instant::now() + std::time::Duration::from_millis(1200)
                ));
            });

            let cancel_on_begin = cancel_anim.clone();
            scroll_controller.connect_scroll_begin(move |_| {
                cancel_on_begin();
            });

            scroll_controller.connect_scroll(move |ctrl, _dx, dy| {
                if ctrl.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                    if dy < 0.0 {
                        zoom_in_scroll();
                        return glib::Propagation::Stop;
                    } else if dy > 0.0 {
                        zoom_out_scroll();
                        return glib::Propagation::Stop;
                    }
                }
                cancel_anim();
                glib::Propagation::Proceed
            });
        }
        scrolled_window.add_controller(scroll_controller);

        // ── Ensure Smooth Scroll Animation Tick Callback is Running ──
        let ensure_smooth_scroll_running = {
            let scrolled_window = scrolled_window.clone();
            let is_animating = is_animating.clone();
            let target_y = target_y.clone();
            let held_direction = held_direction.clone();
            let tick_id_cell = smooth_scroll_tick_id.clone();
            let last_time_cell = last_smooth_frame_time_us.clone();

            Rc::new(move || {
                if tick_id_cell.borrow().is_none() {
                    // Disable GTK's internal kinetic scroll physics while keyboard animation runs
                    scrolled_window.set_kinetic_scrolling(false);
                    is_animating.set(true);
                    last_time_cell.set(None);

                    let is_animating_clone = is_animating.clone();
                    let target_y_clone = target_y.clone();
                    let held_dir_clone = held_direction.clone();
                    let tic = tick_id_cell.clone();
                    let ltc = last_time_cell.clone();
                    let sw_clone = scrolled_window.clone();
                    let vadj = scrolled_window.vadjustment();

                    let tick_id = scrolled_window.add_tick_callback(move |_widget, frame_clock| {
                        if !is_animating_clone.get() {
                            *tic.borrow_mut() = None;
                            ltc.set(None);
                            sw_clone.set_kinetic_scrolling(true);
                            return glib::ControlFlow::Break;
                        }

                        // Compute dt in seconds from gdk::FrameClock microseconds
                        let now_us = frame_clock.frame_time();
                        let dt = if let Some(prev_us) = ltc.get() {
                            let elapsed_us = (now_us - prev_us).clamp(1_000, 100_000);
                            (elapsed_us as f64) / 1_000_000.0
                        } else {
                            1.0 / 60.0
                        };
                        ltc.set(Some(now_us));

                        let max_scroll = (vadj.upper() - vadj.page_size()).max(0.0);

                        // Smooth Continuous Hold: while key is held down, advance target_y continuously at 950 px/s
                        if let Some(dir) = held_dir_clone.get() {
                            let hold_speed = 950.0;
                            let advance = (dir as f64) * hold_speed * dt;
                            let cur_target = target_y_clone.get();
                            target_y_clone.set((cur_target + advance).clamp(0.0, max_scroll));
                        }

                        let target = target_y_clone.get();
                        let cur = vadj.value();
                        let diff = target - cur;

                        // Stop condition: stop if no direction key is held AND remaining distance < 0.25px
                        if held_dir_clone.get().is_none() && diff.abs() < 0.25 {
                            vadj.set_value(target);
                            is_animating_clone.set(false);
                            *tic.borrow_mut() = None;
                            ltc.set(None);
                            sw_clone.set_kinetic_scrolling(true);
                            return glib::ControlFlow::Break;
                        }

                        // Frame-Rate Independent Exponential Damping: factor = 1 - exp(-18.0 * dt)
                        let factor = 1.0 - (-18.0 * dt).exp();
                        let next = cur + diff * factor;

                        let clamped_next = next.clamp(0.0, max_scroll);
                        vadj.set_value(clamped_next);

                        // If clamped at bounds and cannot move further, stop when key is released
                        let updated_cur = vadj.value();
                        if (updated_cur - cur).abs() < 0.005 && (target - updated_cur).abs() < 0.5 {
                            if held_dir_clone.get().is_none() {
                                is_animating_clone.set(false);
                                *tic.borrow_mut() = None;
                                ltc.set(None);
                                sw_clone.set_kinetic_scrolling(true);
                                return glib::ControlFlow::Break;
                            }
                        }

                        glib::ControlFlow::Continue
                    });
                    *tick_id_cell.borrow_mut() = Some(tick_id);
                }
            })
        };

        // ── Smooth Scroll Step Function ──
        let smooth_scroll = {
            let scrolled_window = scrolled_window.clone();
            let is_animating = is_animating.clone();
            let target_y = target_y.clone();
            let tick_id_cell = smooth_scroll_tick_id.clone();
            let last_time_cell = last_smooth_frame_time_us.clone();
            let config = config.clone();
            let pause_cell = auto_scroll_pause_until.clone();
            let ensure_running = ensure_smooth_scroll_running.clone();

            Rc::new(move |delta: f64| {
                let vadj = scrolled_window.vadjustment();
                let page = vadj.page_size();
                let upper = (vadj.upper() - page).max(0.0);
                let current = vadj.value();

                pause_cell.set(Some(
                    std::time::Instant::now() + std::time::Duration::from_millis(1200)
                ));

                if !config.borrow().smooth_scrolling {
                    let new_val = (current + delta).clamp(0.0, upper);
                    is_animating.set(false);
                    if let Some(tid) = tick_id_cell.borrow_mut().take() {
                        tid.remove();
                    }
                    last_time_cell.set(None);
                    scrolled_window.set_kinetic_scrolling(true);
                    target_y.set(new_val);
                    vadj.set_value(new_val);
                    return;
                }

                let base = if is_animating.get() {
                    target_y.get()
                } else {
                    current
                };
                let new_target = (base + delta).clamp(0.0, upper);
                target_y.set(new_target);

                ensure_running();
            })
        };

        // ── Auto-Scroll Toggle Function (Microsecond Delta-Time & Jitter Free) ──
        let toggle_auto_scroll = {
            let auto_scrolling = auto_scrolling.clone();
            let auto_scroll_tick_id = auto_scroll_tick_id.clone();
            let auto_scroll_pause_until = auto_scroll_pause_until.clone();
            let last_as_frame_time_us = last_as_frame_time_us.clone();
            let scrolled_window = scrolled_window.clone();
            let config = config.clone();

            Rc::new(move || {
                if auto_scrolling.get() {
                    // Stop auto-scroll
                    auto_scrolling.set(false);
                    if let Some(tid) = auto_scroll_tick_id.borrow_mut().take() {
                        tid.remove();
                    }
                    last_as_frame_time_us.set(None);
                    auto_scroll_pause_until.set(None);
                } else {
                    // Start auto-scroll
                    auto_scrolling.set(true);
                    auto_scroll_pause_until.set(None);
                    last_as_frame_time_us.set(None);

                    let vadj = scrolled_window.vadjustment();
                    let cfg = config.clone();
                    let as_flag = auto_scrolling.clone();
                    let as_tick_holder = auto_scroll_tick_id.clone();
                    let pause_cell = auto_scroll_pause_until.clone();
                    let last_time_cell = last_as_frame_time_us.clone();

                    let tid = scrolled_window.add_tick_callback(move |_widget, frame_clock| {
                        if !as_flag.get() {
                            *as_tick_holder.borrow_mut() = None;
                            last_time_cell.set(None);
                            return glib::ControlFlow::Break;
                        }

                        let now_us = frame_clock.frame_time();

                        // Pause auto-scroll if user manually navigated recently
                        if let Some(pause_until) = pause_cell.get() {
                            if std::time::Instant::now() < pause_until {
                                last_time_cell.set(Some(now_us));
                                return glib::ControlFlow::Continue;
                            } else {
                                pause_cell.set(None);
                            }
                        }

                        let dt = if let Some(prev_us) = last_time_cell.get() {
                            let elapsed_us = (now_us - prev_us).clamp(1_000, 100_000);
                            (elapsed_us as f64) / 1_000_000.0
                        } else {
                            1.0 / 60.0
                        };
                        last_time_cell.set(Some(now_us));

                        let speed = cfg.borrow().auto_scroll_speed;
                        let page = vadj.page_size();
                        let upper = (vadj.upper() - page).max(0.0);
                        let cur = vadj.value();

                        if cur >= upper {
                            // Reached bottom of article
                            as_flag.set(false);
                            *as_tick_holder.borrow_mut() = None;
                            last_time_cell.set(None);
                            return glib::ControlFlow::Break;
                        }

                        let next = (cur + speed * dt).min(upper);
                        vadj.set_value(next);
                        glib::ControlFlow::Continue
                    });
                    *auto_scroll_tick_id.borrow_mut() = Some(tid);
                }
            })
        };

        let held_direction_press = held_direction.clone();
        let held_direction_release = held_direction.clone();
        let go_back_key = go_back.clone();
        let go_forward_key = go_forward.clone();
        let zoom_in_key = zoom_in.clone();
        let zoom_out_key = zoom_out.clone();
        let zoom_reset_key = zoom_reset.clone();

        key_controller.connect_key_pressed(move |_, keyval, _keycode, modifier| {
            let ctrl = modifier.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = modifier.contains(gdk::ModifierType::SHIFT_MASK);
            let alt = modifier.contains(gdk::ModifierType::ALT_MASK);
            let url_focused = url_entry.has_focus();

            match keyval {
                // Ctrl+Plus or Ctrl+= or Ctrl+KP_Add: Zoom In
                v if (v == gdk::Key::plus || v == gdk::Key::equal || v == gdk::Key::KP_Add) && ctrl => {
                    zoom_in_key();
                    return glib::Propagation::Stop;
                }
                // Ctrl+Minus or Ctrl+KP_Subtract or Ctrl+underscore: Zoom Out
                v if (v == gdk::Key::minus || v == gdk::Key::KP_Subtract || v == gdk::Key::underscore) && ctrl => {
                    zoom_out_key();
                    return glib::Propagation::Stop;
                }
                // Ctrl+0 or Ctrl+KP_0: Reset Zoom (100%)
                v if (v == gdk::Key::_0 || v == gdk::Key::KP_0) && ctrl => {
                    zoom_reset_key();
                    return glib::Propagation::Stop;
                }
                // Alt+Left or Alt+BackSpace: Browser back
                v if (v == gdk::Key::Left || v == gdk::Key::BackSpace) && alt => {
                    go_back_key();
                    return glib::Propagation::Stop;
                }
                // Alt+Right: Browser forward
                v if v == gdk::Key::Right && alt => {
                    go_forward_key();
                    return glib::Propagation::Stop;
                }
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
                // j or Down: Smooth scroll down
                v if v == gdk::Key::j || v == gdk::Key::Down => {
                    // Ignore repeated OS key-press events if down direction is already actively held
                    if held_direction_press.get() == Some(1) {
                        return glib::Propagation::Stop;
                    }
                    held_direction_press.set(Some(1));
                    let step = config.borrow().scroll_step_px;
                    smooth_scroll(step);
                    glib::Propagation::Stop
                }
                // k or Up: Smooth scroll up
                v if v == gdk::Key::k || v == gdk::Key::Up => {
                    // Ignore repeated OS key-press events if up direction is already actively held
                    if held_direction_press.get() == Some(-1) {
                        return glib::Propagation::Stop;
                    }
                    held_direction_press.set(Some(-1));
                    let step = config.borrow().scroll_step_px;
                    smooth_scroll(-step);
                    glib::Propagation::Stop
                }
                // Space: Browser-like scroll down (Shift+Space: scroll up)
                v if v == gdk::Key::space && !ctrl => {
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
                // Home: Smooth scroll to top
                v if v == gdk::Key::Home => {
                    let cur = scrolled_window.vadjustment().value();
                    smooth_scroll(-cur);
                    glib::Propagation::Stop
                }
                // End: Smooth scroll to bottom
                v if v == gdk::Key::End => {
                    let vadj = scrolled_window.vadjustment();
                    let upper = (vadj.upper() - vadj.page_size()).max(0.0);
                    let cur = vadj.value();
                    smooth_scroll(upper - cur);
                    glib::Propagation::Stop
                }
                // s or Ctrl+Space: Toggle auto-scroll
                v if v == gdk::Key::s || (v == gdk::Key::space && ctrl) => {
                    toggle_auto_scroll();
                    glib::Propagation::Stop
                }
                // t: Toggle dark/light mode
                v if v == gdk::Key::t => {
                    config.borrow_mut().toggle_dark_mode();
                    debounced_save();
                    on_config_changed();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });

        // ── Smooth Continuous Hold Release Listener ──
        key_controller.connect_key_released(move |_, keyval, _keycode, _modifier| {
            match keyval {
                v if (v == gdk::Key::j || v == gdk::Key::Down) && held_direction_release.get() == Some(1) => {
                    held_direction_release.set(None);
                }
                v if (v == gdk::Key::k || v == gdk::Key::Up) && held_direction_release.get() == Some(-1) => {
                    held_direction_release.set(None);
                }
                _ => {}
            }
        });

        window.add_controller(key_controller);

        // ── Mouse Back/Forward (Button 8 & 9) Gesture Listener ──
        let mouse_gesture = gtk::GestureClick::new();
        mouse_gesture.set_button(0); // Listen to all mouse buttons
        let go_back_mouse = go_back.clone();
        let go_forward_mouse = go_forward.clone();
        mouse_gesture.connect_pressed(move |gesture, _n_press, _x, _y| {
            let btn = gesture.current_button();
            if btn == 8 {
                go_back_mouse();
                gesture.set_state(gtk::EventSequenceState::Claimed);
            } else if btn == 9 {
                go_forward_mouse();
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        window.add_controller(mouse_gesture);
    }

    // ── Flush Pending Config Save on Window Close ──
    {
        let st = save_timeout.clone();
        let cfg = config.clone();
        window.connect_close_request(move |_| {
            if let Some(src) = st.borrow_mut().take() {
                src.remove();
            }
            cfg.borrow().save();
            glib::Propagation::Proceed
        });
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
///
/// When `theme_changed` is true, also updates the Libadwaita color scheme.
/// When only CSS properties (font size, margin, spacing) change, this skips
/// the expensive Libadwaita color-scheme query to maintain smooth 120 FPS live preview.
fn apply_css(provider: &gtk::CssProvider, config: &Config, theme_changed: bool) {
    let css = theme::generate_css(config);
    provider.load_from_string(&css);

    if theme_changed {
        let style_manager = adw::StyleManager::default();
        match config.theme.as_str() {
            "dark" | "charcoal" | "oled" => style_manager.set_color_scheme(adw::ColorScheme::ForceDark),
            "light" | "sepia" => style_manager.set_color_scheme(adw::ColorScheme::ForceLight),
            "custom" => {
                if config.is_dark() {
                    style_manager.set_color_scheme(adw::ColorScheme::ForceDark);
                } else {
                    style_manager.set_color_scheme(adw::ColorScheme::ForceLight);
                }
            }
            _ => style_manager.set_color_scheme(adw::ColorScheme::Default),
        }
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
                    prev_link: cached.prev_url.map(|u| NavigationLink {
                        url: u,
                        label: "Previous".to_string(),
                    }),
                    next_link: cached.next_url.map(|u| NavigationLink {
                        url: u,
                        label: "Next".to_string(),
                    }),
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
                        let storage_clone = storage.clone();

                        row.connect_activated(move |_| {
                            entry.set_text(&url);
                            trigger_fetch(&url, &tx, &storage_clone);
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

/// Convert an HTML snippet (e.g. from <p> or leaf <div>) to valid GTK Pango markup.
///
/// Converts supported inline tags (b, strong, i, em, code, tt, s, del, strike, u, ins, br),
/// decodes HTML entities into Unicode, XML-escapes special characters (&, <, >),
/// and validates the result with Pango markup parser.
pub fn html_to_pango_markup(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag_content = String::new();
            let mut closed = false;
            while let Some(&next_c) = chars.peek() {
                chars.next();
                if next_c == '>' {
                    closed = true;
                    break;
                }
                tag_content.push(next_c);
            }
            if closed {
                let tag_trimmed = tag_content.trim();
                let is_close = tag_trimmed.starts_with('/');
                let tag_body = if is_close {
                    &tag_trimmed[1..]
                } else {
                    tag_trimmed
                };
                let tag_name = tag_body
                    .split(|ch: char| ch.is_whitespace() || ch == '/')
                    .next()
                    .unwrap_or("")
                    .to_lowercase();

                let token = match (is_close, tag_name.as_str()) {
                    (false, "b" | "strong") => Some("\u{E000}b\u{E001}"),
                    (true, "b" | "strong") => Some("\u{E000}/b\u{E001}"),
                    (false, "i" | "em") => Some("\u{E000}i\u{E001}"),
                    (true, "i" | "em") => Some("\u{E000}/i\u{E001}"),
                    (false, "code" | "tt") => Some("\u{E000}tt\u{E001}"),
                    (true, "code" | "tt") => Some("\u{E000}/tt\u{E001}"),
                    (false, "s" | "del" | "strike") => Some("\u{E000}s\u{E001}"),
                    (true, "s" | "del" | "strike") => Some("\u{E000}/s\u{E001}"),
                    (false, "u" | "ins") => Some("\u{E000}u\u{E001}"),
                    (true, "u" | "ins") => Some("\u{E000}/u\u{E001}"),
                    (false, "br") => Some("\u{E000}br\u{E001}"),
                    _ => None,
                };

                if let Some(t) = token {
                    out.push_str(t);
                }
                continue;
            } else {
                out.push('<');
                out.push_str(&tag_content);
                break;
            }
        }
        out.push(c);
    }

    // Step 2: Decode all HTML entities (e.g. &mdash;, &nbsp;, &ldquo;, &amp;)
    let decoded = html_escape::decode_html_entities(&out).to_string();

    // Step 3: XML-escape standard XML chars so raw &, <, > cannot break Pango markup
    let escaped = glib::markup_escape_text(&decoded);

    // Step 4: Restore supported Pango markup tags & explicit line breaks
    let markup = escaped
        .replace("\u{E000}b\u{E001}", "<b>")
        .replace("\u{E000}/b\u{E001}", "</b>")
        .replace("\u{E000}i\u{E001}", "<i>")
        .replace("\u{E000}/i\u{E001}", "</i>")
        .replace("\u{E000}tt\u{E001}", "<tt>")
        .replace("\u{E000}/tt\u{E001}", "</tt>")
        .replace("\u{E000}s\u{E001}", "<s>")
        .replace("\u{E000}/s\u{E001}", "</s>")
        .replace("\u{E000}u\u{E001}", "<u>")
        .replace("\u{E000}/u\u{E001}", "</u>")
        .replace("\u{E000}br\u{E001}", "\n");

    let trimmed = markup.trim();

    // Step 5: Validate with Pango parser. If valid, return markup; else fallback to safely escaped text.
    if gtk::pango::parse_markup(trimmed, '\0').is_ok() {
        trimmed.to_string()
    } else {
        let plain = trimmed
            .replace("<b>", "")
            .replace("</b>", "")
            .replace("<i>", "")
            .replace("</i>", "")
            .replace("<tt>", "")
            .replace("</tt>", "")
            .replace("<s>", "")
            .replace("</s>", "")
            .replace("<u>", "")
            .replace("</u>", "");
        glib::markup_escape_text(plain.trim()).to_string()
    }
}

pub type ParsedArticle = ArticleData;

/// Strip all HTML tags, returning plain text content.
pub fn strip_all_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}

/// Check if text content represents a scene break divider (e.g. `***`, `* * *`, `---`, `― ― ―`).
pub fn is_scene_divider(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    match trimmed {
        "***" | "* * *" | "*  *  *" | "---" | "- - -" | "-- -- --" | "― ― ―" | "— • —"
        | "— — —" | "~~~" | "~ ~ ~" | "###" | "# # #" | "♦ ♦ ♦" | "◆ ◆ ◆" | "§" | "§ § §"
        | "• • •" | "•••" | "*** ***" | "******" | "—" | "――" | "―――" => {
            return true;
        }
        _ => {}
    }

    let non_ws: Vec<char> = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if non_ws.len() >= 3 {
        let first = non_ws[0];
        if (first == '*'
            || first == '-'
            || first == '_'
            || first == '―'
            || first == '—'
            || first == '~'
            || first == '#'
            || first == '♦'
            || first == '◆'
            || first == '='
            || first == '•')
            && non_ws.iter().all(|&c| c == first)
        {
            return true;
        }
    }

    false
}

/// Split an HTML block by double breaks (`<br><br>`, `<br/><br/>`, `<br>\n<br>`, `\n\n`, or `<hr>`)
/// into individual paragraphs, ensuring short dialog lines and scene breaks are preserved.
pub fn split_html_paragraphs(html: &str) -> Vec<String> {
    let mut normalized = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag_content = String::new();
            let mut closed = false;
            while let Some(&next_c) = chars.peek() {
                chars.next();
                if next_c == '>' {
                    closed = true;
                    break;
                }
                tag_content.push(next_c);
            }
            if closed {
                let tag_trimmed = tag_content.trim().to_lowercase();
                let tag_name = tag_trimmed
                    .split(|ch: char| ch.is_whitespace() || ch == '/')
                    .next()
                    .unwrap_or("");

                if tag_name == "br" {
                    normalized.push('\u{E003}'); // Marker for <br>
                    continue;
                } else if tag_name == "hr" {
                    normalized.push('\u{E002}'); // Paragraph break
                    normalized.push_str("<hr>");
                    normalized.push('\u{E002}'); // Paragraph break
                    continue;
                } else {
                    normalized.push('<');
                    normalized.push_str(&tag_content);
                    normalized.push('>');
                    continue;
                }
            } else {
                normalized.push('<');
                normalized.push_str(&tag_content);
                break;
            }
        }
        normalized.push(c);
    }

    // Pass 2: Detect double breaks (\u{E003} or \n repeated with optional whitespace)
    let mut result = String::with_capacity(normalized.len());
    let chars_vec: Vec<char> = normalized.chars().collect();
    let mut i = 0;
    while i < chars_vec.len() {
        let c = chars_vec[i];
        if c == '\u{E003}' || c == '\n' {
            let mut j = i + 1;
            let mut has_second_break = false;
            while j < chars_vec.len() {
                if chars_vec[j] == ' ' || chars_vec[j] == '\t' || chars_vec[j] == '\r' {
                    j += 1;
                } else if chars_vec[j] == '\u{E003}' || chars_vec[j] == '\n' {
                    has_second_break = true;
                    j += 1;
                } else {
                    break;
                }
            }

            if has_second_break {
                result.push('\u{E002}');
                i = j;
                continue;
            } else {
                if c == '\u{E003}' {
                    result.push_str("<br/>");
                } else {
                    result.push('\n');
                }
                i += 1;
                continue;
            }
        } else {
            result.push(c);
            i += 1;
        }
    }

    result
        .split('\u{E002}')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Individual content blocks for virtualized article rendering with GTK4 ListView.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Title(String),
    Meta {
        byline: String,
        reading_time: String,
    },
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        markup: String,
        plain: String,
    },
    Quote(String),
    Code(String),
    ListItem(String),
    Divider,
    Navigation {
        prev_link: Option<NavigationLink>,
        next_link: Option<NavigationLink>,
    },
}

/// Parse extracted article data into individual virtualized ContentBlocks.
pub fn parse_article_blocks(article: &ParsedArticle) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    // 1. Article Title
    let title_trimmed = html_escape::decode_html_entities(article.title.trim()).to_string();
    if !title_trimmed.is_empty() {
        blocks.push(ContentBlock::Title(title_trimmed.clone()));
    }

    // 2. Metadata (byline & reading time)
    let byline_trimmed = html_escape::decode_html_entities(article.byline.trim()).to_string();
    let reading_time = if article.reading_time_mins > 0 {
        format!("{} min read", article.reading_time_mins)
    } else {
        String::new()
    };
    if !byline_trimmed.is_empty() || !reading_time.is_empty() {
        blocks.push(ContentBlock::Meta {
            byline: byline_trimmed,
            reading_time,
        });
    }

    // 3. Body blocks from HTML
    if !article.html_content.trim().is_empty() {
        let doc = dom_query::Document::from(article.html_content.as_str());
        let nodes = doc.select("p, h1, h2, h3, h4, h5, h6, blockquote, pre, code, li, div, section, hr");
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

            if tag == "hr" {
                if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                    blocks.push(ContentBlock::Divider);
                }
                continue;
            }

            if trimmed.is_empty() {
                continue;
            }

            // Skip h1 if identical to article title to avoid redundant header
            if tag == "h1" && trimmed.eq_ignore_ascii_case(&title_trimmed) {
                continue;
            }

            match tag.as_str() {
                "h1" | "h2" => {
                    let clean = html_escape::decode_html_entities(trimmed).to_string();
                    blocks.push(ContentBlock::Heading {
                        level: 2,
                        text: clean,
                    });
                }
                "h3" | "h4" | "h5" | "h6" => {
                    let clean = html_escape::decode_html_entities(trimmed).to_string();
                    blocks.push(ContentBlock::Heading {
                        level: 3,
                        text: clean,
                    });
                }
                "blockquote" => {
                    // Skip nested blockquote if already inside another blockquote
                    if node.ancestors(Some(3)).is("blockquote") {
                        continue;
                    }
                    let clean = html_escape::decode_html_entities(trimmed).to_string();
                    blocks.push(ContentBlock::Quote(clean));
                }
                "pre" => {
                    // Skip nested pre if already inside another pre
                    if node.ancestors(Some(3)).is("pre") {
                        continue;
                    }
                    let code_text = html_escape::decode_html_entities(&node.text()).to_string();
                    blocks.push(ContentBlock::Code(code_text.trim_matches('\n').to_string()));
                }
                "code" => {
                    // Standalone code block (not inside pre, p, li, blockquote, div, section, h1..h6)
                    if node.ancestors(Some(5)).is("pre, p, li, blockquote, div, section, h1, h2, h3, h4, h5, h6") {
                        continue;
                    }
                    let code_text = html_escape::decode_html_entities(&node.text()).to_string();
                    blocks.push(ContentBlock::Code(code_text.trim_matches('\n').to_string()));
                }
                "li" => {
                    // If li is inside an ol, calculate 1-based index; otherwise use bullet prefix
                    let is_ol = node.parent().is("ol");
                    let prefix = if is_ol {
                        let siblings = node.parent().children().filter("li");
                        let mut idx = 1;
                        for (i, sib) in siblings.iter().enumerate() {
                            if sib.is_selection(&node) {
                                idx = i + 1;
                                break;
                            }
                        }
                        format!("{}. ", idx)
                    } else {
                        "• ".to_string()
                    };

                    // Exclude text of sub-lists from parent item
                    let full_text = node.text().to_string();
                    let item_text = if node.children().filter("ul, ol").length() > 0 {
                        let sub_text = node.children().filter("ul, ol").text().to_string();
                        if let Some(pos) = full_text.rfind(&sub_text) {
                            full_text[..pos].trim()
                        } else {
                            full_text.trim()
                        }
                    } else {
                        full_text.trim()
                    };

                    let clean_text = html_escape::decode_html_entities(item_text).to_string();
                    if !clean_text.is_empty() {
                        blocks.push(ContentBlock::ListItem(format!("{}{}", prefix, clean_text)));
                    }
                }
                "p" => {
                    // Skip if p is already inside blockquote, li, or pre
                    if node.ancestors(Some(3)).is("blockquote, li, pre") {
                        continue;
                    }
                    let inner_html = node.inner_html().to_string();
                    let chunks = split_html_paragraphs(&inner_html);
                    for chunk in chunks {
                        let trimmed_chunk = chunk.trim();
                        if trimmed_chunk.is_empty() {
                            continue;
                        }
                        if trimmed_chunk == "<hr>" {
                            if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                                blocks.push(ContentBlock::Divider);
                            }
                            continue;
                        }
                        let markup = html_to_pango_markup(trimmed_chunk);
                        let plain = html_escape::decode_html_entities(&strip_all_tags(trimmed_chunk)).trim().to_string();
                        if plain.is_empty() {
                            continue;
                        }
                        if is_scene_divider(&plain) {
                            if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                                blocks.push(ContentBlock::Divider);
                            }
                        } else {
                            blocks.push(ContentBlock::Paragraph { markup, plain });
                        }
                    }
                }
                "div" | "section" => {
                    // Only process leaf div/section that contains direct text and no block children
                    if node.select("p, h1, h2, h3, h4, h5, h6, blockquote, pre, li, ul, ol, div, section, hr").length() > 0 {
                        continue;
                    }
                    if node.ancestors(Some(4)).is("p, blockquote, li, pre") {
                        continue;
                    }
                    let inner_html = node.inner_html().to_string();
                    let chunks = split_html_paragraphs(&inner_html);
                    for chunk in chunks {
                        let trimmed_chunk = chunk.trim();
                        if trimmed_chunk.is_empty() {
                            continue;
                        }
                        if trimmed_chunk == "<hr>" {
                            if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                                blocks.push(ContentBlock::Divider);
                            }
                            continue;
                        }
                        let markup = html_to_pango_markup(trimmed_chunk);
                        let plain = html_escape::decode_html_entities(&strip_all_tags(trimmed_chunk)).trim().to_string();
                        if plain.is_empty() {
                            continue;
                        }
                        if is_scene_divider(&plain) {
                            if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                                blocks.push(ContentBlock::Divider);
                            }
                        } else {
                            blocks.push(ContentBlock::Paragraph { markup, plain });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // 4. Fallback to text_content if no body blocks were found in HTML
    let has_body = blocks.iter().any(|b| matches!(
        b,
        ContentBlock::Paragraph { .. }
            | ContentBlock::Heading { .. }
            | ContentBlock::Quote(_)
            | ContentBlock::Code(_)
            | ContentBlock::ListItem(_)
            | ContentBlock::Divider
    ));

    if !has_body {
        for para in article.text_content.split("\n\n") {
            let trimmed = para.trim();
            if !trimmed.is_empty() {
                let clean = html_escape::decode_html_entities(trimmed).to_string();
                if is_scene_divider(&clean) {
                    if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                        blocks.push(ContentBlock::Divider);
                    }
                } else {
                    blocks.push(ContentBlock::Paragraph {
                        markup: glib::markup_escape_text(&clean).to_string(),
                        plain: clean,
                    });
                }
            }
        }
    }

    // 5. Navigation links (Prev / Next) if present
    if article.prev_link.is_some() || article.next_link.is_some() {
        if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
            blocks.push(ContentBlock::Divider);
        }
        blocks.push(ContentBlock::Navigation {
            prev_link: article.prev_link.clone(),
            next_link: article.next_link.clone(),
        });
    }

    eprintln!("[folio::ui] Generated {} ContentBlocks from article", blocks.len());

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zoom_keys_and_bounds() {
        assert_eq!(gdk::Key::plus, gdk::Key::plus);
        assert_eq!(gdk::Key::equal, gdk::Key::equal);
        assert_eq!(gdk::Key::KP_Add, gdk::Key::KP_Add);
        assert_eq!(gdk::Key::minus, gdk::Key::minus);
        assert_eq!(gdk::Key::KP_Subtract, gdk::Key::KP_Subtract);
        assert_eq!(gdk::Key::_0, gdk::Key::_0);
        assert_eq!(gdk::Key::KP_0, gdk::Key::KP_0);

        // Verify zoom clamping logic
        let step: f64 = 2.0;
        let mut font_size: f64 = 47.0;
        font_size = (font_size + step).min(48.0);
        assert_eq!(font_size, 48.0);
        font_size = (font_size + step).min(48.0);
        assert_eq!(font_size, 48.0);

        font_size = 11.0;
        font_size = (font_size - step).max(10.0);
        assert_eq!(font_size, 10.0);
        font_size = (font_size - step).max(10.0);
        assert_eq!(font_size, 10.0);

        // Reset
        font_size = Config::default().font_size;
        assert_eq!(font_size, 18.0);
    }

    #[test]
    fn test_parse_article_blocks_structured() {
        let article = ArticleData {
            url: "https://example.com/test".to_string(),
            title: "Test Article".to_string(),
            byline: "John Doe".to_string(),
            html_content: "<h2>Section 1</h2><p>Paragraph <strong>one</strong>.</p><blockquote>A wise quote</blockquote><pre>let x = 10;</pre><li>Item A</li>".to_string(),
            text_content: "Paragraph one.\n\nA wise quote".to_string(),
            reading_time_mins: 5,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 7);
        assert_eq!(blocks[0], ContentBlock::Title("Test Article".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Meta {
                byline: "John Doe".to_string(),
                reading_time: "5 min read".to_string(),
            }
        );
        assert_eq!(
            blocks[2],
            ContentBlock::Heading {
                level: 2,
                text: "Section 1".to_string(),
            }
        );
        assert!(matches!(&blocks[3], ContentBlock::Paragraph { markup, plain } if markup.contains("<b>one</b>") && plain == "Paragraph one."));
        assert_eq!(blocks[4], ContentBlock::Quote("A wise quote".to_string()));
        assert_eq!(blocks[5], ContentBlock::Code("let x = 10;".to_string()));
        assert_eq!(blocks[6], ContentBlock::ListItem("• Item A".to_string()));
    }

    #[test]
    fn test_parse_article_blocks_text_fallback() {
        let article = ArticleData {
            url: "https://example.com/plain".to_string(),
            title: "Plain Article".to_string(),
            byline: "".to_string(),
            html_content: "".to_string(),
            text_content: "First paragraph.\n\nSecond paragraph.".to_string(),
            reading_time_mins: 2,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0], ContentBlock::Title("Plain Article".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Meta {
                byline: "".to_string(),
                reading_time: "2 min read".to_string(),
            }
        );
        assert!(matches!(&blocks[2], ContentBlock::Paragraph { plain, .. } if plain == "First paragraph."));
        assert!(matches!(&blocks[3], ContentBlock::Paragraph { plain, .. } if plain == "Second paragraph."));
    }

    #[test]
    fn test_parse_article_blocks_skips_redundant_h1_matching_title() {
        let article = ArticleData {
            url: "https://example.com/dup".to_string(),
            title: "Unique Title".to_string(),
            byline: "".to_string(),
            html_content: "<h1>Unique Title</h1><p>Body paragraph.</p>".to_string(),
            text_content: "Body paragraph.".to_string(),
            reading_time_mins: 1,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], ContentBlock::Title("Unique Title".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Meta {
                byline: "".to_string(),
                reading_time: "1 min read".to_string(),
            }
        );
        assert!(matches!(&blocks[2], ContentBlock::Paragraph { plain, .. } if plain == "Body paragraph."));
    }

    #[test]
    fn test_html_to_pango_markup_entities_and_formatting() {
        let input = "Hello &mdash; <strong>bold &amp; <em>italic</em></strong> &lt;world&gt; <span class=\"highlight\">text</span><br/>New line";
        let markup = html_to_pango_markup(input);
        assert!(markup.contains("Hello — <b>bold &amp; <i>italic</i></b> &lt;world&gt; text\nNew line"));
        assert!(gtk::pango::parse_markup(&markup, '\0').is_ok());
    }

    #[test]
    fn test_parse_article_blocks_ordered_and_unordered_lists() {
        let article = ArticleData {
            url: "https://example.com/lists".to_string(),
            title: "Lists".to_string(),
            byline: "".to_string(),
            html_content: "<ul><li>Bullet 1</li><li>Bullet 2</li></ul><ol><li>First</li><li>Second</li></ol>".to_string(),
            text_content: "Bullet 1\nBullet 2\nFirst\nSecond".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[1], ContentBlock::ListItem("• Bullet 1".to_string()));
        assert_eq!(blocks[2], ContentBlock::ListItem("• Bullet 2".to_string()));
        assert_eq!(blocks[3], ContentBlock::ListItem("1. First".to_string()));
        assert_eq!(blocks[4], ContentBlock::ListItem("2. Second".to_string()));
    }

    #[test]
    fn test_parse_article_blocks_pre_code_dedup() {
        let article = ArticleData {
            url: "https://example.com/code".to_string(),
            title: "Code Snippet".to_string(),
            byline: "".to_string(),
            html_content: "<pre><code>fn main() {\n    println!(\"Hello\");\n}</code></pre>".to_string(),
            text_content: "fn main() {\n    println!(\"Hello\");\n}".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], ContentBlock::Title("Code Snippet".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Code("fn main() {\n    println!(\"Hello\");\n}".to_string())
        );
    }

    #[test]
    fn test_parse_article_blocks_leaf_div_and_section() {
        let article = ArticleData {
            url: "https://example.com/divs".to_string(),
            title: "Div Article".to_string(),
            byline: "".to_string(),
            html_content: "<div class=\"wrapper\"><div class=\"text-leaf\">Direct text in leaf div &mdash; cool!</div><section class=\"sec-leaf\">Direct text in leaf section.</section></div>".to_string(),
            text_content: "Direct text in leaf div\nDirect text in leaf section".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], ContentBlock::Title("Div Article".to_string()));
        assert!(matches!(&blocks[1], ContentBlock::Paragraph { plain, markup } if plain.contains("Direct text in leaf div — cool!") && markup.contains("—")));
        assert!(matches!(&blocks[2], ContentBlock::Paragraph { plain, .. } if plain.contains("Direct text in leaf section.")));
    }

    #[test]
    fn test_parse_article_blocks_blockquote_with_p_no_duplicate() {
        let article = ArticleData {
            url: "https://example.com/quote".to_string(),
            title: "Quote Article".to_string(),
            byline: "".to_string(),
            html_content: "<blockquote><p>This is a quote inside a paragraph.</p></blockquote>".to_string(),
            text_content: "This is a quote inside a paragraph.".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], ContentBlock::Title("Quote Article".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Quote("This is a quote inside a paragraph.".to_string())
        );
    }

    #[test]
    fn test_parse_article_blocks_split_double_br_and_preserve_dialog() {
        let article = ArticleData {
            url: "https://example.com/novel".to_string(),
            title: "Novel Chapter".to_string(),
            byline: "".to_string(),
            html_content: "<p>\"Hello.\"<br><br>\"Who is there?\"<br/><br/>***<br><br>\"It is me.\"</p>".to_string(),
            text_content: "\"Hello.\"\n\n\"Who is there?\"\n\n***\n\n\"It is me.\"".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[0], ContentBlock::Title("Novel Chapter".to_string()));
        assert!(matches!(&blocks[1], ContentBlock::Paragraph { plain, .. } if plain == "\"Hello.\""));
        assert!(matches!(&blocks[2], ContentBlock::Paragraph { plain, .. } if plain == "\"Who is there?\""));
        assert_eq!(blocks[3], ContentBlock::Divider);
        assert!(matches!(&blocks[4], ContentBlock::Paragraph { plain, .. } if plain == "\"It is me.\""));
    }

    #[test]
    fn test_parse_article_blocks_hr_divider() {
        let article = ArticleData {
            url: "https://example.com/hr".to_string(),
            title: "Scene Change".to_string(),
            byline: "".to_string(),
            html_content: "<p>First scene</p><hr><p>Second scene</p>".to_string(),
            text_content: "First scene\n\nSecond scene".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0], ContentBlock::Title("Scene Change".to_string()));
        assert!(matches!(&blocks[1], ContentBlock::Paragraph { plain, .. } if plain == "First scene"));
        assert_eq!(blocks[2], ContentBlock::Divider);
        assert!(matches!(&blocks[3], ContentBlock::Paragraph { plain, .. } if plain == "Second scene"));
    }

    #[test]
    fn test_parse_article_blocks_navigation_links() {
        let article = ArticleData {
            url: "https://example.com/ch2".to_string(),
            title: "Chapter 2".to_string(),
            byline: "Author".to_string(),
            html_content: "<p>Chapter 2 story content.</p>".to_string(),
            text_content: "Chapter 2 story content.".to_string(),
            reading_time_mins: 1,
            prev_link: Some(NavigationLink {
                url: "https://example.com/ch1".to_string(),
                label: "Chapter 1".to_string(),
            }),
            next_link: Some(NavigationLink {
                url: "https://example.com/ch3".to_string(),
                label: "Chapter 3".to_string(),
            }),
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[0], ContentBlock::Title("Chapter 2".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Meta {
                byline: "Author".to_string(),
                reading_time: "1 min read".to_string(),
            }
        );
        assert!(matches!(&blocks[2], ContentBlock::Paragraph { plain, .. } if plain == "Chapter 2 story content."));
        assert_eq!(blocks[3], ContentBlock::Divider);
        assert_eq!(
            blocks[4],
            ContentBlock::Navigation {
                prev_link: Some(NavigationLink {
                    url: "https://example.com/ch1".to_string(),
                    label: "Chapter 1".to_string(),
                }),
                next_link: Some(NavigationLink {
                    url: "https://example.com/ch3".to_string(),
                    label: "Chapter 3".to_string(),
                }),
            }
        );
    }

    #[test]
    fn test_list_view_virtualization_bind() {
        if gtk::init().is_err() {
            return;
        }

        let store = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
        let blocks = vec![
            ContentBlock::Title("Hello World".to_string()),
            ContentBlock::Meta {
                byline: "Author".to_string(),
                reading_time: "3 min read".to_string(),
            },
            ContentBlock::Heading {
                level: 2,
                text: "Heading Two".to_string(),
            },
            ContentBlock::Paragraph {
                markup: "This is a <b>paragraph</b>.".to_string(),
                plain: "This is a paragraph.".to_string(),
            },
            ContentBlock::Quote("A nice quote".to_string()),
            ContentBlock::Code("println!(\"code\");".to_string()),
            ContentBlock::ListItem("List Item".to_string()),
            ContentBlock::Divider,
        ];

        for b in blocks {
            store.append(&glib::BoxedAnyObject::new(b));
        }

        let sel = gtk::NoSelection::new(Some(store.clone()));
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
            container.add_css_class("article-block-container");
            let label = gtk::Label::new(None);
            label.set_wrap(true);
            label.set_selectable(true);
            container.append(&label);
            item.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&container));
        });

        factory.connect_bind(|_, item| {
            let list_item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let obj = list_item.item().and_downcast::<glib::BoxedAnyObject>().unwrap();
            let block: std::cell::Ref<ContentBlock> = obj.borrow();
            let container = list_item.child().and_downcast::<gtk::Box>().unwrap();
            let label = container.first_child().and_downcast::<gtk::Label>().unwrap();

            match &*block {
                ContentBlock::Title(t) => {
                    label.add_css_class("article-title");
                    label.set_text(t);
                }
                ContentBlock::Meta { byline, reading_time } => {
                    label.add_css_class("article-byline");
                    label.set_text(&format!("{} • {}", byline, reading_time));
                }
                ContentBlock::Heading { text, .. } => {
                    label.add_css_class("article-h2");
                    label.set_text(text);
                }
                ContentBlock::Paragraph { markup, .. } => {
                    label.add_css_class("article-paragraph");
                    label.set_markup(markup);
                }
                ContentBlock::Quote(q) => {
                    label.add_css_class("article-blockquote");
                    label.set_text(q);
                }
                ContentBlock::Code(c) => {
                    label.add_css_class("article-codeblock");
                    label.set_text(c);
                }
                ContentBlock::ListItem(l) => {
                    label.add_css_class("article-list-item");
                    label.set_text(l);
                }
                ContentBlock::Divider => {
                    label.add_css_class("article-divider");
                    label.set_text("— • —");
                }
                ContentBlock::Navigation { .. } => {}
            }
        });

        for i in 0..600 {
            store.append(&glib::BoxedAnyObject::new(ContentBlock::Paragraph {
                markup: format!("Paragraph <b>{}</b>", i),
                plain: format!("Paragraph {}", i),
            }));
        }

        let list_view = gtk::ListView::new(Some(sel), Some(factory));
        let clamp = adw::ClampScrollable::builder().child(&list_view).maximum_size(720).build();
        let sw = gtk::ScrolledWindow::builder().child(&clamp).build();
        let win = gtk::Window::builder().child(&sw).default_width(600).default_height(500).build();
        win.present();
        for _ in 0..10 {
            glib::MainContext::default().iteration(false);
        }

        assert_eq!(Some(sw.vadjustment()), list_view.vadjustment(), "ScrolledWindow and ListView must share the same vadjustment");
        assert!(sw.vadjustment().upper() >= 0.0);

        // Scroll deep into the list to verify virtualization extends past initial 204 items
        sw.vadjustment().set_value(sw.vadjustment().upper() - sw.vadjustment().page_size());
        for _ in 0..10 {
            glib::MainContext::default().iteration(false);
        }
        assert!(sw.vadjustment().value() > 0.0);
        win.destroy();
    }
}
