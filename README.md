# macrdp

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![macOS](https://img.shields.io/badge/macOS-14%2B-black.svg)](https://www.apple.com/macos/)
[![Apple Silicon](https://img.shields.io/badge/Apple%20Silicon-Supported-green.svg)](#)

English | **[中文](README_ZH.md)**

**macOS Remote Desktop Server**

A native RDP server for macOS. Remote into your Mac from Windows, Linux, iOS, or Android — using any standard RDP client like Windows Remote Desktop (mstsc), Microsoft Remote Desktop, or FreeRDP.

> **Why macrdp?** macOS has no built-in RDP server. VNC is slow and blurry. macrdp gives your Mac a first-class remote desktop experience — fast, sharp, and compatible with every RDP client out of the box.

---

## Features

- **Standard RDP protocol** — works with any RDP client, no special software needed on the client side
- **Hardware-accelerated encoding** — GPU-powered H.264 via Apple VideoToolbox, low latency on Apple Silicon
- **High fidelity color** — AVC444 mode for pixel-perfect color reproduction (RDP 10)
- **Full keyboard & mouse** — complete input injection with 104-key mapping, numpad, modifiers, scroll
- **HiDPI / Retina support** — capture at 2x/3x resolution for sharp 4K remote display
- **Dynamic resolution** — automatically follows client window resize; the server reacts to display-control PDUs and adjusts the session resolution on the fly
- **Native cursor embedding** — real macOS cursor shapes (resize handles, I-beams, etc.) are streamed in the video; configurable via `show_cursor`
- **Display sleep tolerance** — server starts and accepts connections even when the Mac display is asleep; the display is woken automatically on the first client connect
- **Configurable** — resolution, frame rate, bitrate, encoder, quality presets, all via simple TOML config
- **Secure** — NLA/CredSSP authentication with auto-generated TLS certificates
- **Lock screen capture** — automatic CoreGraphics fallback when the screen is locked
- **Single-client v1** — one active RDP session is supported; concurrent sessions are not supported

---

## Requirements

- **macOS 14+** (Sonoma or later)
- **Rust 1.75+**
- Screen Recording permission (System Settings > Privacy & Security)
- Accessibility permission (for keyboard/mouse injection)

---

## Quick Start

**Option A — pre-built binary (Apple Silicon)**

Download the latest `macrdp-server-*-aarch64-apple-darwin.tar.gz` from [GitHub Releases](https://github.com/tasleson/macrdp/releases), then:

```bash
tar -xzf macrdp-server-*-aarch64-apple-darwin.tar.gz
./macrdp-server
```

**Option B — build from source**

```bash
cargo build --release
cargo run --release --bin macrdp-server
```

Connect from any RDP client → `your-mac-ip:3389`

macrdp v1 supports one active RDP client at a time. Starting a second concurrent
session is unsupported; disconnect the active client before reconnecting from
another device.

---

## Configuration

Copy `config.example.toml` to `config.toml` and edit as needed:

```toml
# Network
port = 13389
bind_address = "0.0.0.0"

# Authentication
username = "admin"
password = "123456"
allow_generated_credentials = false

# Display
width = 0          # 0 = auto-detect
height = 0
frame_rate = 60
hidpi_scale = 2    # 2x for 4K on Retina
show_cursor = true  # embed macOS cursor shapes in video stream

# Encoding
quality = "high_quality"    # low_latency / balanced / high_quality
encoder = "hardware"        # hardware (GPU) / software (CPU)
chroma_mode = "avc420"      # avc420 (compatible) / avc444 (best quality)
bitrate_mbps = 50           # target bitrate (Mbps)

# Logging
log_level = "info"          # trace / debug / info / warn / error
log_path = "/path/to/macrdp.log"
```

All daemon files live under a single base directory:

- macOS: `~/Library/Application Support/macrdp/`
- Linux/BSD: `$XDG_CONFIG_HOME/macrdp/` (or `~/.config/macrdp/`)

Default layout:

| File                     | Purpose                                       |
| ------------------------ | --------------------------------------------- |
| `<base>/config.toml`     | Daemon configuration                          |
| `<base>/tls/cert.pem`    | TLS certificate (auto-generated if missing)   |
| `<base>/tls/key.pem`     | TLS private key (auto-generated, mode `0600`) |
| `<base>/logs/macrdp.log` | Daemon log                                    |

Each path can be overridden via the matching config field (`cert_path`, `key_path`, `log_path`) or CLI flag (`--cert-path`, `--key-path`, `--log-path`).

### Keychain password storage

Instead of storing the RDP password as plain text in `config.toml`, you can store it in the macOS Keychain:

```bash
# Store (prompts with no echo; --username defaults to $USER if omitted)
macrdp-server --keychain-set-password --username alice

# Start the server reading the password from Keychain
macrdp-server --password-keychain
```

The entry is stored as a generic password with service `macrdp` and account equal to the RDP username. To manage it with standard macOS tools:

```bash
# Update — just re-run the set command; it overwrites the existing entry
macrdp-server --keychain-set-password --username alice

# Delete via the security CLI
security delete-generic-password -s macrdp -a alice

# Inspect
security find-generic-password -s macrdp -a alice
```

You can also view or delete the entry in **Keychain Access.app** by searching for "macrdp".

---

## Running as a launchd LaunchAgent

macrdp ships a sample plist at `packaging/launchd/com.macrdp.daemon.plist`. It is designed for a per-user **LaunchAgent** (not a system LaunchDaemon) because macOS Screen Recording and Accessibility permissions are tied to the logged-in GUI session.

**Install**

```bash
# 1. Build a release binary and put it somewhere persistent
cargo build --release
sudo install -m 0755 target/release/macrdp-server /usr/local/bin/macrdp-server

# 2. Lay down config + log directories
mkdir -p "$HOME/Library/Application Support/macrdp/logs"
cp config.example.toml "$HOME/Library/Application Support/macrdp/config.toml"
# Edit the config — at minimum set username/password (or allow_generated_credentials)

# 3. Materialize the plist with absolute paths (no tilde expansion in plists)
mkdir -p "$HOME/Library/LaunchAgents"
sed \
  -e "s|__MACRDP_BIN__|/usr/local/bin/macrdp-server|g" \
  -e "s|__HOME__|$HOME|g" \
  packaging/launchd/com.macrdp.daemon.plist \
  > "$HOME/Library/LaunchAgents/com.macrdp.daemon.plist"

# 4. Load and start
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.macrdp.daemon.plist"
launchctl kickstart -k "gui/$(id -u)/com.macrdp.daemon"
```

The first time launchd starts the daemon, macOS will prompt the user session for **Screen Recording** and **Accessibility** permission against the `macrdp-server` binary. Grant both in System Settings > Privacy & Security and then restart the service.

**Status, stop, restart**

```bash
launchctl print "gui/$(id -u)/com.macrdp.daemon"          # status + last exit
launchctl kill SIGTERM "gui/$(id -u)/com.macrdp.daemon"   # graceful stop (auto-restarted if crashed)
launchctl kickstart -k "gui/$(id -u)/com.macrdp.daemon"   # force restart
```

**Uninstall**

```bash
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.macrdp.daemon.plist"
rm "$HOME/Library/LaunchAgents/com.macrdp.daemon.plist"
# Optionally remove state:
# rm -rf "$HOME/Library/Application Support/macrdp"
# sudo rm /usr/local/bin/macrdp-server
```

The plist uses `KeepAlive = { SuccessfulExit = false; Crashed = true; }` so a clean SIGTERM (e.g. from `launchctl bootout`) stays stopped, while a crash is retried after `ThrottleInterval` (10s).

---

## Project Structure

```
crates/
├── macrdp-server/       Main server binary
├── macrdp-capture/      Screen capture
├── macrdp-input/        Keyboard & mouse injection
├── macrdp-encode/       Video encoding
├── ironrdp-server-gfx/  RDP protocol (IronRDP fork)
└── ironrdp-acceptor-patched/
                         RDP connection acceptor
```

---

## Acknowledgments

This project stands on the shoulders of giants. Special thanks to:

- **[IronRDP](https://github.com/Devolutions/IronRDP)** — Pure Rust RDP protocol implementation. macrdp's protocol stack is built on a fork of ironrdp-server with GFX/AVC444 extensions.
- **[FreeRDP](https://github.com/FreeRDP/FreeRDP)** — The reference open-source RDP implementation. Its AVC444 dual-stream encoding approach and YUV444 B-area split algorithm were essential references.
- **[RustDesk](https://github.com/rustdesk/rustdesk)** — Open-source remote desktop software written in Rust. Its architecture for cross-platform screen capture and input injection was a great source of inspiration.

---

## License

This project is licensed under the **GNU General Public License v3.0** — see [LICENSE](LICENSE) for details. Any derivative work must also be distributed under GPLv3.

---

<details>
<summary><b>Keywords</b></summary>

macOS RDP server, Mac remote desktop server, RDP server for Mac, remote desktop protocol macOS, connect to Mac from Windows, connect to Mac from Linux, connect to Mac from Android, Windows Remote Desktop to Mac, mstsc Mac, Mac remote access, Mac screen sharing, remote control Mac, Apple Silicon remote desktop, Rust RDP server, VNC alternative Mac, FreeRDP Mac server, macOS remote desktop, Mac remote desktop solution, RDP for macOS

</details>
