use crate::{detect, hitsounds, launch_record, mods, pak, pak_tweaks, scalability, wav_to_wem};

#[tauri::command]
pub(crate) fn get_scalability_path() -> Result<String, String> {
    scalability::get_scalability_path()
}

#[tauri::command]
pub(crate) fn read_scalability(path: String) -> Result<String, String> {
    scalability::read_scalability(&path)
}

#[tauri::command]
pub(crate) fn write_scalability(path: String, content: String) -> Result<(), String> {
    scalability::write_scalability(&path, &content)
}

#[tauri::command]
pub(crate) fn detect_install_path() -> Option<detect::InstallInfo> {
    detect::detect_game_install()
}

#[tauri::command]
pub(crate) fn list_pak_files(game_root: String) -> Result<Vec<String>, String> {
    pak::list_pak_files(&game_root)
}

#[tauri::command]
pub(crate) fn list_pak_files_info(game_root: String) -> Result<Vec<pak::PakFileInfo>, String> {
    pak::list_pak_files_info(&game_root)
}

#[tauri::command]
pub(crate) async fn list_pak_contents(pak_path: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pak::list_pak_contents(&pak_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn unpack_pak(
    pak_path: String,
    output_dir: String,
    skip: Vec<String>,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let skip_refs: Vec<&str> = skip.iter().map(|s| s.as_str()).collect();
        pak::unpack_pak(&pak_path, &output_dir, &skip_refs)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_single_file(
    pak_path: String,
    file_name: String,
    output_path: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::extract_single_file(&pak_path, &file_name, &output_path)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn repack_pak(input_dir: String, output_pak: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || pak::repack_pak(&input_dir, &output_pak))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn repack_iostore(input_dir: String, output_utoc: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || pak::repack_iostore(&input_dir, &output_utoc))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn list_utoc_contents(utoc_path: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pak::list_utoc_contents(&utoc_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_utoc(
    utoc_path: String,
    output_dir: String,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pak::extract_utoc(&utoc_path, &output_dir))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_utoc_file(
    utoc_path: String,
    file_name: String,
    output_path: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::extract_utoc_file(&utoc_path, &file_name, &output_path)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn count_utoc_legacy_packages(
    utoc_path: String,
    game_root: String,
    filter: Vec<String>,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::count_utoc_legacy_packages(&utoc_path, &game_root, &filter)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_utoc_legacy(
    utoc_path: String,
    game_root: String,
    output_dir: String,
    filter: Vec<String>,
    app: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::extract_utoc_legacy(&utoc_path, &game_root, &output_dir, &filter, app)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn cancel_legacy_extraction() {
    pak::cancel_legacy_extraction();
}

#[tauri::command]
pub(crate) fn get_mods_status(game_root: String) -> mods::ModsStatus {
    mods::get_mods_status(&game_root)
}

#[tauri::command]
pub(crate) fn install_signature_bypass(game_root: String) -> Result<String, String> {
    mods::install_signature_bypass(&game_root)
}

#[tauri::command]
pub(crate) fn remove_signature_bypass(game_root: String) -> Result<String, String> {
    mods::remove_signature_bypass(&game_root)
}

#[tauri::command]
pub(crate) fn open_mods_folder(game_root: String) -> Result<(), String> {
    mods::open_mods_folder(&game_root)
}

#[tauri::command]
pub(crate) fn get_tweak_definitions() -> Vec<scalability::TweakDefinition> {
    scalability::get_tweak_definitions()
}

#[tauri::command]
pub(crate) fn detect_tweaks(content: String) -> Vec<scalability::TweakState> {
    scalability::detect_tweaks(&content)
}

#[tauri::command]
pub(crate) fn apply_tweaks(content: String, settings: Vec<scalability::TweakSetting>) -> String {
    scalability::apply_tweaks(&content, &settings)
}

#[tauri::command]
pub(crate) async fn inspect_pak_path(
    pak_path: String,
) -> Result<Option<pak_tweaks::PakIniInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::inspect_single_pak(&pak_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn scan_mod_paks_for_ini(
    game_root: String,
) -> Result<Vec<pak_tweaks::PakIniInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::scan_mod_paks(&game_root))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn detect_pak_tweaks(
    pak_path: String,
) -> Result<Vec<scalability::TweakState>, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::detect_pak_tweaks(&pak_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn apply_pak_tweak_edits(
    pak_path: String,
    edits: Vec<pak_tweaks::PakTweakEdit>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::apply_pak_tweaks(&pak_path, &edits))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_pak_ini(pak_path: String, entry: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::extract_pak_ini(&pak_path, &entry))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn save_pak_ini(
    pak_path: String,
    files: Vec<pak_tweaks::PakIniFileContent>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::save_pak_ini(&pak_path, files))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn clear_shader_cache() -> Result<String, String> {
    scalability::clear_shader_cache()
}

#[tauri::command]
pub(crate) fn launch_game(install_info: detect::InstallInfo) -> Result<(), String> {
    install_info.launch_game()
}

#[tauri::command]
pub(crate) fn get_skip_launcher(game_root: String) -> Result<bool, String> {
    launch_record::get_skip_launcher(&game_root)
}

#[tauri::command]
pub(crate) fn set_skip_launcher(game_root: String, skip: bool) -> Result<(), String> {
    launch_record::set_skip_launcher(&game_root, skip)
}

#[tauri::command]
pub(crate) fn toggle_mod_enabled(
    mods_folder: String,
    full_name: String,
    enabled: bool,
) -> Result<(), String> {
    mods::toggle_mod_enabled(&mods_folder, &full_name, enabled)
}

#[tauri::command]
pub(crate) async fn export_mods_zip(
    mods_folder: String,
    dest_path: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || mods::export_mods_zip(&mods_folder, &dest_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn delete_mod(mods_folder: String, full_name: String) -> Result<(), String> {
    mods::delete_mod(&mods_folder, &full_name)
}

#[tauri::command]
pub(crate) fn install_mod(
    mods_folder: String,
    source_path: String,
) -> Result<mods::InstallResult, String> {
    mods::install_mod(&mods_folder, &source_path)
}

#[tauri::command]
pub(crate) async fn install_from_zip(
    mods_folder: String,
    zip_path: String,
) -> Result<Vec<mods::InstallResult>, String> {
    tauri::async_runtime::spawn_blocking(move || mods::install_from_zip(&mods_folder, &zip_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn validate_wav(path: String) -> Result<wav_to_wem::WavValidation, String> {
    wav_to_wem::validate_wav(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn path_exists(path: String) -> bool {
    std::path::Path::new(&path).exists()
}

#[tauri::command]
pub(crate) async fn build_hitsound_mod(
    game_root: String,
    head_wav: Option<String>,
    body_wav: Option<String>,
    mod_name: String,
    output_dir: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        hitsounds::build_hitsound_mod_to_dir(
            &game_root,
            head_wav.as_deref(),
            body_wav.as_deref(),
            &mod_name,
            &output_dir,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}
