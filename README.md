# oMLX Menubar

A lightweight macOS menu bar app for controlling your [oMLX](https://github.com/nicehash/omlx) LLM server. Start, stop, switch models, and monitor status — no terminal needed.

![Menu bar badge](src-tauri/icons/tray-on.png) ← What it looks like in your menu bar.

## Features

- **Start / Stop / Restart** oMLX via the menu bar
- **Model switching** — see available models, load any with one click
- **Auto-load** — loads your default model (or the first available) on start
- **Status badge** — filled chip when running, outlined when stopped, animated fill on startup
- **Dashboard shortcut** — opens the oMLX admin UI in your browser on start
- **Settings panel** — configure API URL, key, paths, and default model in-app
- **Launch at Login** — toggle from the menu

## Requirements

- macOS (Apple Silicon)
- [oMLX](https://github.com/nicehash/omlx) installed and configured with a launchd plist
- Rust toolchain + Tauri CLI (for building from source)

## Install

### From DMG

Download the latest `.dmg` from [Releases](../../releases), open it, drag to Applications.

### From source

```bash
# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install Tauri CLI
cargo install tauri-cli --version "^2"

# Clone and build
git clone https://github.com/YOUR_USERNAME/omlx-menubar.git
cd omlx-menubar/src-tauri
cargo tauri build
```

The `.app` bundle lands in `src-tauri/target/release/bundle/macos/`.

## Configuration

On first launch, a config file is created at `~/.config/omlx-menubar/config.json`:

```json
{
  "api_url": "http://127.0.0.1:8000/v1",
  "api_key": "",
  "service_label": "ai.omlx.server",
  "plist_path": "~/Library/LaunchAgents/ai.omlx.server.plist",
  "dashboard_url": "http://127.0.0.1:8000/admin",
  "default_model": ""
}
```

| Field | Description |
|-------|-------------|
| `api_url` | oMLX OpenAI-compatible API endpoint |
| `api_key` | Your oMLX API key (leave empty if none set) |
| `service_label` | The launchd service name from your plist |
| `plist_path` | Full path to your oMLX launchd plist |
| `dashboard_url` | Opened in your browser when oMLX starts |
| `default_model` | Auto-loaded on start. Empty = first available model. |

You can also edit these from the app via **Preferences…** in the menu.

## How it works

The app controls oMLX through `launchctl` (start/stop) and the oMLX admin API (model management). It doesn't spawn oMLX as a child process — it manages the existing launchd service you already have configured.

## A note on this project

This app was AI-generated (built with Claude). I'm not a Rust developer — I don't know if this code is considered good by Rust standards. What I do know is that the app works, it saves me time, and it solves a real problem I had. I'm sharing it in case others find it useful too.

If you're a Rust developer and see things that could be improved, PRs are welcome. If you just want a menu bar app to control oMLX, grab the DMG and go.

## License

MIT
