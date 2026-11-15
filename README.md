# rex-mdpeek

Cross-platform **Markdown & [Pele](https://pele.run) SVG Diagram Viewer** for **Rex** (and any Kitty-Graphics-capable terminal such as Ghostty, Kitty, or WezTerm) across **Windows, Linux, FreeBSD, macOS, and remote SSH hosts**.

## Why `rex-mdpeek`?

AI coding agents running inside terminal panes frequently generate Markdown explanations (`file:///.../*.md`) with accompanying Mermaid / Pele SVG diagrams (`./diagrams/<slug>.svg`). Leaving the terminal to open a browser or external Markdown previewer breaks flow—and OS-specific GUI panels break cross-platform or SSH workflows.

`rex-mdpeek` renders CommonMark/GFM documents **and** crisp vector-rasterized `.svg` diagrams right inside a floating Rex overlay (`mdpeek_toggle`) or docked right-hand split (`mdpeek_dock`):

- **100% OS-Agnostic**: Single self-contained Rust binary with zero native GUI framework dependencies. Runs identically on **Windows, Linux, FreeBSD, macOS, and remote SSH sessions**.
- **<25 ms Instant Launch**: Bundles open-source `Roboto` + `Fira Code` fonts directly in the binary (`include_bytes!`)—zero system font directory scan on startup and identical glyph metrics on every OS.
- **60 fps Zero-Reupload Scrolling**: Uploads each rasterized diagram texture to terminal GPU memory **once** using zlib-compressed Kitty Graphics (`a=t,f=32,o=z`), then scrolls and clips scanlines in hardware via 80-byte source-rectangle placements (`a=p,x=0,y=...,w=...,h=...`).
- **Rich Markdown + Pele Support**:
  - Headings, GitHub Alerts (`[!NOTE]`, `[!TIP]`, `[!IMPORTANT]`, `[!WARNING]`, `[!CAUTION]`), tables, task lists, blockquotes, and code fences
  - Sibling SVG/PNG/WebP diagrams (`![title](./diagrams/foo.svg)`) and inline ```` ```mermaid ```` fences (auto-compiled via `~/.agents/skills/pele` when installed)
  - Interactive `<details><summary>Mermaid source</summary>` folding (`d` / `Enter` / mouse click)
- **Live Chat Discovery**: Captures the active Rex chat pane scrollback (`rex capture`), reconstructs soft-wrapped Unix & Windows `file://` links, and opens the newest `.md` automatically (with live `Follow` mode for docked splits).

---

## Installation

### All Platforms (Windows, Linux, macOS, FreeBSD) via Cargo

```sh
cargo install --git https://github.com/verte21/rex-mdpeek
rex-mdpeek install
```

`rex-mdpeek install` copies the binary into `~/.local/bin` (or `~/.cargo/bin` on Windows), writes `mdpeek.lua` into your Rex config directory (`~/.config/rex/mdpeek.lua` or `%REX_CONFIG_DIR%`), wires `require("mdpeek")` into `init.lua`, and reloads Rex.

### Local Clone (POSIX `sh`)

```sh
git clone https://github.com/verte21/rex-mdpeek.git
cd rex-mdpeek
./install.sh
```

### Standalone Terminal Usage (Ghostty / Kitty / WezTerm without Rex)

```sh
rex-mdpeek path/to/document.md
```

---

## Rex Shortcuts & Commands

| Shortcut / Command | Action |
| :--- | :--- |
| `Shift+Cmd+M` (macOS) / `Ctrl+Shift+M` (Windows) | Toggle floating `92% × 90%` Markdown & Pele viewer (`mdpeek_toggle`) |
| `Alt+Cmd+M` (macOS) / `Ctrl+Alt+M` (Windows) | Toggle right-hand docked Markdown & Pele split (`mdpeek_dock`) |
| `rex do mdpeek_toggle path=/tmp/diagrams/arch.md` | Open a specific Markdown file in the floating overlay |
| `rex-mdpeek open path/to/doc.md [--dock]` | CLI helper to trigger the Rex overlay or split |

---

## Keybindings Inside `rex-mdpeek`

| Key / Mouse | Behavior |
| :--- | :--- |
| `1` / `2` / `3` / `Tab` | Switch discovery scope: **1:Chat** (active terminal scrollback) · **2:Diagrams** (`<temp>/diagrams`) · **3:Workspace** (`cwd`) |
| `j` / `k` / `↓` / `↑` / Wheel | Smooth line-by-line scrolling (hardware Kitty source-rect clipping on diagrams) |
| `Ctrl+D` / `Ctrl+U` / `Space` / `PgDn` / `PgUp` | Half-page & full-page scrolling (`g` / `G` for top / bottom) |
| `[` / `]` or `p` / `n` | Previous / next discovered Markdown file |
| `+` / `-` / `0` | Zoom inline diagrams (`50%`–`220%`, `0` resets to `100%`) |
| `d` / `Enter` / Click `▸` | Expand or collapse `<details>` Mermaid source blocks |
| `b` | Toggle file list sidebar |
| `f` | Toggle live-follow of newly mentioned `.md` files in chat |
| `e` | Open active `.md` in **Zed** / **VS Code** |
| `o` | Open active `.md` in system default viewer (`open` / `xdg-open` / `start`) |
| `r` | Re-sync chat scrollback & reload active Markdown + diagrams |
| `q` / `Esc` | Dismiss viewer and return focus to your chat pane |

---

## Environment Variables

- `REX_MDPEEK_THEME=dark|light` — Force dark (`tokyo-night-storm`) or light (`flexoki-light`) palette on any OS (on macOS, `rex-mdpeek` also syncs with Rex's active theme in the background).
- `REX_MDPEEK_BIN` — Override path to the `rex-mdpeek` executable used by `mdpeek.lua`.
- `REX_BIN` — Override path to the `rex` CLI binary.
- `REX_CONFIG_DIR` — Override Rex configuration directory for `rex-mdpeek install` / `uninstall`.

---

## License

MIT License — see [LICENSE](LICENSE). Bundled font licenses (`Roboto` Apache-2.0, `Fira Code` SIL OFL-1.1) are documented in [assets/fonts/LICENSE-FONTS.txt](assets/fonts/LICENSE-FONTS.txt).
