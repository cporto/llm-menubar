# LLM Menubar

A lightweight macOS menu bar app for managing local LLM servers. Supports [oMLX](https://github.com/nicehash/omlx) and [llama.cpp](https://github.com/ggml-org/llama.cpp) — start, stop, switch models, and monitor status from one place. No terminal needed.

## Features

- **Dual-backend** — switch between oMLX and llama.cpp from the menu bar
- **Start / Stop / Restart Server** — manages your LLM server via launchd
- **Model switching** — see available models, load any with one click (unloads the previous one automatically)
- **Auto-start** — server starts automatically when the app launches
- **Auto-load** — remembers your last-used model and loads it on start
- **Live status** — elapsed timers on all operations (starting, loading, unloading, stopping)
- **Status badge** — filled pill when running, outlined when stopped, animated fill sweep on startup and model loading
- **llama.cpp router mode** — discovers all `.gguf` models in a directory, load/unload on demand
- **llama.cpp single-model mode** — works with `llama-server -m` setups too (auto-detected)
- **Dashboard shortcut** — opens the server's web UI in your browser on start
- **Settings panel** — configure everything in-app (or switch backends directly from the menu)
- **Launch at Login** — toggle from the menu
- **Smart defaults** — switching backends prefills the right ports, labels, and paths

## Requirements

- macOS (Apple Silicon)
- One or both LLM servers installed:
  - [oMLX](https://github.com/nicehash/omlx) with a launchd plist
  - [llama.cpp](https://github.com/ggml-org/llama.cpp) (`brew install llama.cpp`) with a launchd plist
- Rust toolchain + Tauri CLI (for building from source only)

## Install

### From DMG

Download the latest `.dmg` from [Releases](../../releases), open it, drag to Applications.

### From source

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo install tauri-cli --version "^2"

git clone https://github.com/cporto/llm-menubar.git
cd llm-menubar/src-tauri
cargo tauri build
```

The `.app` bundle lands in `src-tauri/target/release/bundle/macos/`.

## Setup

### oMLX

If you already have oMLX running with a launchd plist, the app works out of the box — the defaults match oMLX's standard setup (port 8000, `ai.omlx.server` service label).

### llama.cpp

You'll need a launchd plist to let the app manage the server. Create `~/Library/LaunchAgents/com.llama.server.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.llama.server</string>
    <key>ProgramArguments</key>
    <array>
        <string>/opt/homebrew/bin/llama-server</string>
        <string>--models-dir</string>
        <string>/path/to/your/models</string>
        <string>--host</string>
        <string>127.0.0.1</string>
        <string>--port</string>
        <string>8080</string>
        <string>-ngl</string>
        <string>999</string>
        <string>--models-max</string>
        <string>1</string>
    </array>
    <key>RunAtLoad</key>
    <false/>
    <key>KeepAlive</key>
    <false/>
    <key>StandardOutPath</key>
    <string>/tmp/llama-server.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/llama-server.log</string>
</dict>
</plist>
```

**Tips:**
- Set `--models-dir` to the folder containing your `.gguf` files — the server discovers them automatically
- `--models-max 1` prevents multiple models from loading simultaneously (important for machines with limited RAM)
- `-ngl 999` offloads all layers to GPU (Apple Silicon Metal)
- `RunAtLoad` should be `false` so the app controls when the server starts and stops
- For single-model use, replace `--models-dir` with `-m /path/to/model.gguf` and add `--alias friendly-name`

Then select **llama.cpp** from the Server Type menu in the app — the defaults (port 8080, `com.llama.server`) will be prefilled automatically.

## Configuration

Config lives at `~/.config/omlx-menubar/config.json`:

```json
{
  "server_type": "llamacpp",
  "api_url": "http://127.0.0.1:8080",
  "api_key": "",
  "service_label": "com.llama.server",
  "plist_path": "~/Library/LaunchAgents/com.llama.server.plist",
  "dashboard_url": "http://127.0.0.1:8080",
  "default_model": ""
}
```

| Field | Description |
|-------|-------------|
| `server_type` | `"omlx"` or `"llamacpp"` |
| `api_url` | Server endpoint (oMLX: `http://127.0.0.1:8000/v1`, llama.cpp: `http://127.0.0.1:8080`) |
| `api_key` | oMLX API key (leave empty for llama.cpp — no auth needed) |
| `service_label` | The launchd service name from your plist |
| `plist_path` | Path to your launchd plist (tilde `~` is expanded automatically) |
| `dashboard_url` | Opened in your browser when the server starts |
| `default_model` | Auto-loaded on start. Automatically saved when you switch models. |

You can edit all of these from **Preferences…** in the menu, or switch backends directly from the **Server Type** submenu.

## How it works

The app manages your LLM server through two layers:

- **Server lifecycle** — `launchctl` bootstrap/bootout/kickstart to start and stop the launchd service
- **Model management** — HTTP calls to the server's API to list, load, and unload models

It doesn't spawn the server as a child process — it manages the existing launchd service. This means the server keeps running if you quit the app, and the app can detect a server that's already running on launch.

### Menu bar states

| Pill | Title | Meaning |
|------|-------|---------|
| Outline | *(empty)* | Server stopped |
| Sweep animation | `Starting server… 3s` | Server starting (timer ticking) |
| Solid | `No model loaded` | Server running, pick a model |
| Sweep animation | `Loading model… 5s` | Model loading into memory |
| Solid | `Qwen3-8B-4bit` | Ready — model loaded and serving |

## A note on this project

This app was built with AI assistance (Claude). I'm not a Rust developer — I don't know if this code is considered good by Rust standards. What I do know is that the app works, it saves me time, and it solves a real problem I had. I'm sharing it in case others find it useful too.

If you're a Rust developer and see things that could be improved, PRs are welcome. If you just want a menu bar app to manage your local LLM server, grab the DMG and go.

## License

MIT
