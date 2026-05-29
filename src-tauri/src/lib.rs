//! Rivals Toolkit Tauri backend. Crate root that wires every domain's commands into the Tauri invoke handler.

#![deny(clippy::unwrap_used, clippy::expect_used)]

mod audio;
mod concurrency;
mod detect;
mod game_status;
mod game_user_settings;
mod launch_record;
mod mods;
mod pak;
mod pak_tweaks;
mod paths;
mod scalability;
mod settings;
mod sounds;
mod tweaks;
mod update_check;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[allow(clippy::expect_used)]
pub fn run() {
    concurrency::init_global_pool();
    let loaded_settings = settings::Settings::load();
    game_status::set_check_enabled(loaded_settings.game_running_check_enabled);
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage::<settings::SettingsState>(std::sync::Mutex::new(loaded_settings))
        .manage::<mods::hero_cache::HeroCacheState>(std::sync::Mutex::new(
            mods::hero_cache::HeroCache::load(),
        ))
        .invoke_handler(tauri::generate_handler![
            // detect
            detect::detect_install_path,
            detect::launch_game,
            // paths
            paths::validate_game_path,
            paths::path_exists,
            // game_status
            game_status::get_game_running,
            game_status::get_should_block_for_game,
            launch_record::get_skip_launcher,
            launch_record::set_skip_launcher,
            // settings
            settings::get_recursive_mod_scan,
            settings::set_recursive_mod_scan,
            settings::get_show_hero_icons,
            settings::set_show_hero_icons,
            settings::get_game_path,
            settings::get_saved_install_info,
            settings::set_game_path,
            settings::get_mod_compression_level,
            settings::set_mod_compression_level,
            settings::get_vanilla_compression_level,
            settings::set_vanilla_compression_level,
            settings::get_game_running_check_enabled,
            settings::set_game_running_check_enabled,
            settings::get_mod_conflict_check_enabled,
            settings::set_mod_conflict_check_enabled,
            // update_check
            update_check::check_for_update,
            update_check::get_auto_check_updates,
            update_check::set_auto_check_updates,
            // audio
            audio::validate_wav,
            // sounds
            sounds::build_sound_mod,
            sounds::extract_sound_wavs,
            sounds::load_sound_mod_for_edit,
            // pak
            pak::commands::list_pak_files,
            pak::commands::list_pak_files_info,
            pak::commands::list_pak_contents,
            pak::commands::unpack_pak,
            pak::commands::extract_single_file,
            pak::commands::extract_pak_files,
            pak::commands::extract_utoc_files,
            pak::commands::repack_pak,
            pak::commands::repack_iostore,
            pak::commands::cancel_repack_iostore,
            pak::commands::list_utoc_contents,
            pak::commands::extract_utoc,
            pak::commands::extract_utoc_file,
            pak::commands::count_utoc_legacy_packages,
            pak::commands::extract_utoc_legacy,
            pak::commands::cancel_legacy_extraction,
            pak::commands::extract_vanilla_container,
            pak::commands::cancel_vanilla_extract,
            pak::commands::rebuild_vanilla_container,
            pak::commands::cancel_vanilla_rebuild,
            // pak_tweaks
            pak_tweaks::commands::inspect_pak_path,
            pak_tweaks::commands::scan_mod_paks_for_ini,
            pak_tweaks::commands::detect_pak_tweaks,
            pak_tweaks::commands::apply_pak_tweak_edits,
            pak_tweaks::commands::extract_pak_ini,
            pak_tweaks::commands::extract_game_default_ini,
            pak_tweaks::commands::create_new_mod_pak,
            pak_tweaks::commands::save_pak_ini,
            pak_tweaks::commands::inspect_pak_path_any_ini,
            pak_tweaks::commands::scan_mod_paks_any_ini,
            // scalability
            scalability::commands::get_scalability_path,
            scalability::commands::read_scalability,
            scalability::commands::write_scalability,
            scalability::commands::get_tweak_definitions,
            scalability::commands::detect_tweaks,
            scalability::commands::apply_tweaks,
            game_user_settings::commands::get_game_user_settings_path,
            game_user_settings::commands::read_game_user_settings,
            game_user_settings::commands::write_game_user_settings,
            game_user_settings::commands::get_game_user_settings_definitions,
            game_user_settings::commands::detect_game_user_settings_tweaks,
            game_user_settings::commands::apply_game_user_settings_tweaks,
            tweaks::shader_cache::clear_shader_cache,
            // tweaks/profiles
            tweaks::profiles::list_tweak_profiles,
            tweaks::profiles::save_tweak_profile,
            tweaks::profiles::overwrite_tweak_profile,
            tweaks::profiles::delete_tweak_profile,
            tweaks::profiles::rename_tweak_profile,
            tweaks::profiles::export_tweak_profile,
            tweaks::profiles::export_tweak_profile_to_file,
            tweaks::profiles::import_tweak_profile,
            tweaks::profiles::import_tweak_profile_from_file,
            // mods
            mods::commands::get_mods_status,
            mods::commands::check_mod_conflicts,
            mods::commands::install_signature_bypass,
            mods::commands::remove_signature_bypass,
            mods::commands::is_signature_bypass_installed,
            mods::commands::get_signature_bypass_kind,
            mods::commands::open_mods_folder,
            mods::commands::toggle_mod_enabled,
            mods::commands::toggle_mods_enabled,
            mods::commands::export_mods_archive,
            mods::commands::rename_mod,
            mods::commands::delete_mod,
            mods::commands::delete_mods,
            mods::commands::install_mod,
            mods::commands::install_from_archive,
            // mods/heroes
            mods::heroes::list_known_heroes,
            mods::heroes::rescan_mod_heroes,
            // mods/character_sync
            mods::character_sync::get_character_data_info,
            mods::character_sync::sync_character_data,
            mods::character_sync::should_auto_sync_character_data,
            mods::character_sync::get_auto_sync_character_data,
            mods::character_sync::set_auto_sync_character_data,
            // mods/profiles
            mods::profiles::list_mod_profiles,
            mods::profiles::save_mod_profile,
            mods::profiles::delete_mod_profile,
            mods::profiles::rename_mod_profile,
            mods::profiles::overwrite_mod_profile,
            mods::profiles::preview_mod_profile,
            mods::profiles::apply_mod_profile,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
