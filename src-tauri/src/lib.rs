#![deny(clippy::unwrap_used, clippy::expect_used)]

mod commands;
mod detect;
mod hitsounds;
mod launch_record;
mod mods;
mod ogg_to_wav;
mod pak;
mod pak_tweaks;
mod paths;
mod prefs;
mod scalability;
mod update_check;
mod wav_to_wem;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[allow(clippy::expect_used)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::detect_install_path,
            commands::list_pak_files,
            commands::list_pak_files_info,
            commands::list_pak_contents,
            commands::unpack_pak,
            commands::extract_single_file,
            commands::extract_pak_files,
            commands::extract_utoc_files,
            commands::repack_pak,
            commands::repack_iostore,
            commands::cancel_repack_iostore,
            commands::list_utoc_contents,
            commands::extract_utoc,
            commands::extract_utoc_file,
            commands::count_utoc_legacy_packages,
            commands::extract_utoc_legacy,
            commands::cancel_legacy_extraction,
            commands::get_mods_status,
            commands::install_signature_bypass,
            commands::remove_signature_bypass,
            commands::open_mods_folder,
            commands::get_scalability_path,
            commands::read_scalability,
            commands::write_scalability,
            commands::get_tweak_definitions,
            commands::detect_tweaks,
            commands::apply_tweaks,
            commands::scan_mod_paks_for_ini,
            commands::inspect_pak_path,
            commands::detect_pak_tweaks,
            commands::apply_pak_tweak_edits,
            commands::clear_shader_cache,
            commands::launch_game,
            commands::toggle_mod_enabled,
            commands::export_mods_zip,
            commands::delete_mod,
            commands::install_mod,
            commands::install_from_zip,
            commands::get_skip_launcher,
            commands::set_skip_launcher,
            commands::extract_pak_ini,
            commands::save_pak_ini,
            commands::validate_wav,
            commands::path_exists,
            commands::build_hitsound_mod,
            commands::extract_hitsound_wavs,
            commands::check_for_update,
            commands::get_auto_check_updates,
            commands::set_auto_check_updates
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
