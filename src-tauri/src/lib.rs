mod commands;
mod detect;
mod mods;
mod pak;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::detect_install_path,
            commands::list_pak_files,
            commands::list_pak_contents,
            commands::unpack_pak,
            commands::extract_single_file,
            commands::repack_pak,
            commands::get_mods_status,
            commands::install_signature_bypass,
            commands::open_mods_folder
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
