// Entry point required by Tauri's build; all real logic lives in `lib.rs` so
// it stays testable outside a bundled app context.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    avalon_hub_app_lib::run();
}
