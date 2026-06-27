// cfg_attr applies the attribute only in release builds.
// windows_subsystem = "windows" hides the console on Windows.
// On macOS it's a no-op but standard Tauri boilerplate.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    omlx_menubar::run();
}
