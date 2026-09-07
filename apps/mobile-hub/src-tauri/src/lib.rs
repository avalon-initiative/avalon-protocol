//! Scaffold only — no Avalon-specific native commands yet. Real work (talking
//! to avalon-server, secure token storage on-device) comes with the
//! milestone-1 vertical slice.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running avalon mobile-hub");
}
