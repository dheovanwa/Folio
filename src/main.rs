mod config;
mod fetcher;
mod storage;
mod theme;
mod ui;

use adw::prelude::*;
use clap::Parser;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Parser, Debug)]
#[command(name = "folio", version, about = "A lightweight, distraction-free article reader")]
struct Args {
    /// URL to open immediately
    url: Option<String>,
}

fn main() -> gtk::glib::ExitCode {
    // Memory optimizations:
    // 1. Purge freed heap pages back to the OS immediately instead of keeping cached arenas.
    if std::env::var_os("MIMALLOC_PURGE_DELAY").is_none() {
        unsafe { std::env::set_var("MIMALLOC_PURGE_DELAY", "0"); }
    }
    // 2. Default to cairo 2D renderer if GSK_RENDERER is unset by the user.
    // This avoids loading ~80MB of Nvidia/Vulkan LLVM GPU shader compilers into memory,
    // reduces memory by over 45%, and prevents Vulkan swapchain warnings on Wayland.
    if std::env::var_os("GSK_RENDERER").is_none() {
        unsafe { std::env::set_var("GSK_RENDERER", "cairo"); }
    }

    let args = Args::parse();

    let app = adw::Application::builder()
        .application_id("dev.personal.folio")
        .build();

    let initial_url = args.url;
    app.connect_activate(move |app| {
        ui::window::build_window(app, initial_url.clone());
    });

    app.run_with_args::<&str>(&[])
}
