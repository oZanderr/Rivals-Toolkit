use crate::{detect, mods, pak, scalability};

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
pub(crate) async fn list_pak_contents(pak_path: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pak::list_pak_contents(&pak_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn unpack_pak(
    pak_path: String,
    output_dir: String,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pak::unpack_pak(&pak_path, &output_dir))
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
pub(crate) fn get_mods_status(game_root: String) -> mods::ModsStatus {
    mods::get_mods_status(&game_root)
}

#[tauri::command]
pub(crate) fn install_signature_bypass(game_root: String) -> Result<String, String> {
    mods::install_signature_bypass(&game_root)
}

#[tauri::command]
pub(crate) fn open_mods_folder(game_root: String) -> Result<(), String> {
    mods::open_mods_folder(&game_root)
}
