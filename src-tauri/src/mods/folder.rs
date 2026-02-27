use super::paths::mods_dir;

pub(crate) fn open_mods_folder(game_root: &str) -> Result<(), String> {
    let mods = mods_dir(game_root);
    if !mods.exists() {
        return Err("~mods folder does not exist — install the bypass first.".to_string());
    }
    #[cfg(target_os = "windows")]
    std::process::Command::new("explorer")
        .arg(mods.to_string_lossy().as_ref())
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
