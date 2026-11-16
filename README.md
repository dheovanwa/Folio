# Folio

A fast, lightweight, distraction-free desktop URL article reader for Linux, built with **Rust (Edition 2024)**, **GTK4**, and **Libadwaita**. Optimized for **CachyOS (Arch Linux + Wayland)**.

Folio strips out clutter, advertisements, popups, and layout distractions, presenting articles in a clean, customizable reading canvas with offline caching and keyboard navigation.

---

## Features

- **Distraction-Free Reading:** Pure Rust content extraction powered by `dom_smoothie` (Mozilla Readability algorithm) to extract clean titles, bylines, and main article text.
- **Reading Comfort Drawer (`Aa` / `Ctrl + ,`):**
  - **Color Themes:** Built-in presets for *Light (Paper)*, *Sepia*, *Dark*, *OLED Pure Black*, and fully customizable theme colors.
  - **Typography & Layout:** System font family picker, font size (`14px`–`32px`), line height (`1.2`–`2.2`), max column width (`550px`–`950px`), text margin (`12px`–`120px`), and text alignment (left / justified).
  - **Live Preview:** Instantaneous UI updates with frame throttling and debounced persistence to `~/.config/folio/config.toml`.
- **High-Efficiency Memory Footprint:**
  - Integrated `mimalloc` global allocator with aggressive heap release.
  - Default Cairo GSK rendering to bypass heavy GPU shader compilation overhead on Wayland/Nvidia.
  - Unified selectable rich text label reducing widget counts from hundreds down to 1.
- **Offline Library & History:** Embedded SQLite database (`~/.local/share/folio/library.db`) storing reading history, bookmarks, cached offline HTML, and scroll positions.
- **Browser-Style & Vim Navigation:**
  - `Space` / `Shift + Space`: Scroll down / up by page (with reading overlap).
  - `j` / `k`: Smooth scrolling with ease-out interpolation.
  - `s`: Toggle configurable auto-scroll.
  - `t`: Quick-cycle theme presets.
  - `Ctrl + L`: Focus and select the URL input bar.
  - `Ctrl + ,`: Toggle reading comfort settings drawer.
  - `F11`: Zen Mode / Fullscreen.
  - `Ctrl + A`: Full continuous selection across the entire article.

---

## Installation & Build

### Arch Linux / CachyOS (`PKGBUILD`)

To build and install the native Arch package using `makepkg`:

```bash
# Clone the repository
git clone https://github.com/dheovanwa/Folio.git
cd Folio

# Build and install package with pacman
makepkg -si
```

### Manual Cargo Build

Requirements:
- Rust 1.80+ (Edition 2024)
- `gtk4`, `libadwaita`, `sqlite`

```bash
cargo build --release
./target/release/folio [URL]
```

---

## Keyboard Shortcuts

| Shortcut | Action |
| :--- | :--- |
| `Ctrl + L` | Focus and select URL bar |
| `Ctrl + ,` | Toggle reading comfort drawer |
| `Space` | Page scroll down |
| `Shift + Space` | Page scroll up |
| `j` / `k` | Smooth scroll down / up |
| `s` | Toggle auto-scroll |
| `t` | Quick-cycle theme presets |
| `F11` | Toggle Zen Mode (Fullscreen) |
| `Escape` | Close settings drawer or exit Zen mode |

---

## License

MIT License. See [LICENSE](LICENSE) for details.
