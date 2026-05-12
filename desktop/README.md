# Accord — desktop tray app

A small Tauri 2 app that wraps `accordserver` and a bundled `livekit-server` so
non-developers can install the server with a `.dmg`, `.deb`, or `.msi` and run
it like any other desktop application.

The app has no main window. It lives in the menu bar / system tray and offers:

- **Open in browser** — `http://localhost:39099`
- **Open data folder** — platform data dir (see below)
- **View logs** — `accord.log` in the data folder
- **Enable / disable start on login**
- **Quit Accord** — gracefully stops both sidecars

## Data locations

Generated on first launch:

| Platform | Data dir |
|---|---|
| macOS | `~/Library/Application Support/gg.daccord.Accord/` |
| Linux | `$XDG_DATA_HOME/accord/` (default: `~/.local/share/accord/`) |
| Windows | `%APPDATA%\Accord\Accord\` |

Contents:

- `config.toml` — generated LiveKit API key/secret, TOTP encryption key, ports
- `livekit.yaml` — config consumed by the bundled LiveKit server
- `accord.db` — SQLite database
- `cdn/` — uploaded emoji, avatars, attachments
- `logs/accord.log`, `logs/livekit.log`, `logs/desktop.log*`

Delete the data dir to fully reset; keep it across reinstalls to preserve
state.

## First-run UX

1. User downloads and runs the installer for their platform.
2. App appears in `/Applications` / Start Menu / package manager listing.
3. Launch the app → tray icon appears.
4. Click **Open in browser** → the Godot client (or any REST client) hits
   `http://localhost:39099`.

## Local build

You will need:

- A stable Rust toolchain (`rustup`).
- [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS:
  WebView2 on Windows, WebKitGTK 4.1 + `librsvg` on Linux, Xcode CLI tools on
  macOS.
- Node 20 + pnpm (only for invoking the Tauri CLI).
- Both sidecar binaries staged under `src-tauri/binaries/`, named with the
  Rust target triple suffix Tauri requires:

```
src-tauri/binaries/
  accordserver-<target-triple>[.exe]
  livekit-server-<target-triple>[.exe]
```

A helper script for local staging:

```bash
TARGET=$(rustc -vV | awk '/host:/ {print $2}')
cargo build --release --bin accordserver --manifest-path ../Cargo.toml
cp ../target/release/accordserver src-tauri/binaries/accordserver-$TARGET
# Download LiveKit for your OS/arch from https://github.com/livekit/livekit/releases
cp /path/to/livekit-server src-tauri/binaries/livekit-server-$TARGET
chmod +x src-tauri/binaries/*
```

Then:

```bash
pnpm add -D @tauri-apps/cli   # one-time
pnpm tauri build              # produces installers in src-tauri/target/release/bundle/
pnpm tauri dev                # for iterative development
```

## Icons

The `icons/` directory must contain the platform icon set Tauri references in
`tauri.conf.json`. Generate from a single source PNG:

```
pnpm tauri icon path/to/source.png
```

The CI workflow expects committed icons; until artwork is finalised, commit a
placeholder.

## CI

`.github/workflows/desktop-release.yml` builds installers for four targets on
tag push (`v*`):

| Target | Output |
|---|---|
| `aarch64-apple-darwin` | `.dmg` |
| `x86_64-apple-darwin` | `.dmg` |
| `x86_64-unknown-linux-gnu` | `.deb`, `.AppImage` |
| `x86_64-pc-windows-msvc` | `.msi`, `.exe` (NSIS) |

The workflow downloads the pinned LiveKit release, stages both binaries with
the correct Tauri suffixes, and runs `pnpm tauri build`. Artifacts are
uploaded to the matching GitHub Release.

## Known limitations / follow-ups

- **Unsigned builds** — Gatekeeper (macOS) and SmartScreen (Windows) will warn
  on first launch. Workaround for users: right-click → Open (macOS), "More
  info" → "Run anyway" (Windows). Signing is a separate work item.
- **NAT traversal** — friends connecting from outside the user's LAN need
  port-forwarding for TCP 39099 (chat), TCP 7880/7881 (LiveKit signaling), and
  UDP 50000–60000 (LiveKit media). A Tailscale-style relay is a possible
  future addition.
- **Auto-update** — the Tauri updater plugin is not wired yet; it requires
  signed builds.
- **Settings UI** — currently the only settings surface is editing
  `config.toml` by hand. A small webview for port / LiveKit key regeneration
  / connected-user stats could be added.
