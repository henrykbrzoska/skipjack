# Skipjack 🐟⏭

[![CI](https://github.com/henrykbrzoska/skipjack/actions/workflows/ci.yml/badge.svg)](https://github.com/henrykbrzoska/skipjack/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/henrykbrzoska/skipjack)](https://github.com/henrykbrzoska/skipjack/releases/latest)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE.md)

**Mute & skip YouTube ads on your TV** — Smart TVs, Chromecast / Google TV,
Fire TV, game consoles… Skipjack joins the YouTube app as a remote via the
YouTube *Lounge* API ("Link with TV code") and automatically mutes and skips
ad breaks while you watch.

## ✨ Features

- 🔇 **Mute ads** — volume drops to 0 for the ad break, restores after
- ⏭ **Skip ads** — presses *Skip* as soon as YouTube allows it
- 📺 **Multi-TV** — pair several TVs, each handled independently
- 🔍 **Auto-discovery** — finds TVs on your LAN via DIAL + Chromecast/mDNS
- 🖥 **Desktop GUI** — pairing wizard, device list, live ad status, log viewer
- 🐳 **Headless daemon** — runs 24/7 on a server or Raspberry Pi

## 📥 Download

Grab the latest release for your OS from the
[**Releases page**](https://github.com/henrykbrzoska/skipjack/releases/latest):

| OS | CLI (`skipjack`) | Desktop app |
|----|------------------|-------------|
| 🐧 Linux x86_64 | `skipjack-linux-x86_64` | `.AppImage` / `.deb` |
| 🪟 Windows x86_64 | `skipjack-windows-x86_64.exe` | `.msi` / `setup.exe` |
| 🍎 macOS Apple Silicon | `skipjack-macos-aarch64` | `.dmg` |
| 🍎 macOS Intel | `skipjack-macos-x86_64` | (build from source) |

No release yet? See [Build from source](#-build-from-source).

## 🚀 Quick start (headless)

```sh
# 1. Pair your TV: open YouTube on the TV → Settings → Link with TV code
skipjack pair 123456789012

# 2. Or discover TVs on your LAN automatically
skipjack discover

# 3. Start blocking ads (Ctrl+C to stop)
skipjack
```

Full CLI reference:

```sh
skipjack setup        # interactive first-time setup
skipjack pair CODE    # pair via "Link with TV code"
skipjack discover     # find TVs on the LAN (DIAL + Chromecast)
skipjack              # start the daemon
skipjack config-path  # print config file location
skipjack log-path     # print log file location
```

Options: `--data <dir>` / `SKIPJACK_DATA_DIR` to override the data dir,
`--debug` for verbose console output.

## 🖥 Desktop app

Pairing wizard → device list → live status (playing / ad / muted) → log viewer.
Everything the CLI does, with buttons.

```sh
cd desktop
npm install
npm run dev      # development
npm run bundle   # installers / bundles
```

## ⚙️ Configuration

`~/.local/share/skipjack/config.json` (or the platform equivalent —
`skipjack config-path` prints it):

```json
{
  "devices": [{ "screen_id": "…", "name": "Living room TV" }],
  "mute_ads": true,
  "skip_ads": true,
  "auto_play": true,
  "join_name": "Skipjack",
  "use_proxy": false
}
```

| Key | Default | What |
|-----|---------|------|
| `mute_ads` | `true` | Mute volume during ad breaks |
| `skip_ads` | `true` | Press *Skip* as soon as possible |
| `auto_play` | `true` | Auto-resume playback if an ad pauses it |
| `join_name` | `"Skipjack"` | Name shown on the TV as connected device |
| `use_proxy` | `false` | Route Lounge traffic through a proxy |

## 📋 Logs

Every run appends to `<data dir>/logs/skipjack.log` (`skipjack log-path`
prints it), always at debug level: every Lounge event from the TV, every
command sent (`→ setVolume …`) with the HTTP answer (`← setVolume: HTTP 200`),
and ad decisions. Ad events (`onAdStateChange`, `adPlaying`) are also printed
on the console. The file rotates to `skipjack.log.1` above 10 MB.
`SKIPJACK_LOG=trace` also dumps the raw long-poll stream.

## 🧱 Project layout

| Path | What |
|------|------|
| `crates/skipjack-core` | Engine: config, Lounge client, ad handling, service, logging, DIAL + Chromecast discovery |
| `crates/skipjack-cli` | `skipjack` binary: daemon (`start`), `setup`, `discover`, `pair`, `config-path`, `log-path` |
| `desktop/` | Angular 22 frontend (zoneless, signals) |
| `desktop/src-tauri` | Tauri 2 backend embedding the core engine |

## 🔨 Build from source

Requirements: Rust (stable), Node 22, and on Linux
`libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf`.

```sh
cargo build --release -p skipjack-cli   # → target/release/skipjack
cargo test --workspace                  # run tests
cargo clippy --workspace --all-targets  # lint
```

## 🤝 Contributing

PRs welcome! Run `cargo fmt`, `cargo clippy` and `cargo test` before pushing —
CI checks all three on Linux, Windows and macOS.

## 📄 License

GPL-3.0-or-later. See [LICENSE.md](LICENSE.md).
