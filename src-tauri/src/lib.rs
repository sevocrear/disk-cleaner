#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(commands::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::list_disks,
            commands::list_trash,
            commands::restore_trash,
            commands::delete_trash,
            commands::empty_trash,
            commands::start_scan,
            commands::apply_selected,
            commands::format_bytes_cmd,
            commands::open_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
