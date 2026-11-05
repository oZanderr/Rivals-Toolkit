//! Unreal shader cache cleanup. Recommended after applying any rendering-related tweaks

use std::fs;

use tauri::State;

use crate::paths::saved_dir;
use crate::settings::SettingsState;

fn clear_shader_cache_files(game_root: Option<&str>) -> Result<String, String> {
    let base = saved_dir(game_root)
        .ok_or_else(|| "Could not find the game's Saved folder.".to_string())?;
    let mut cleared = 0usize;

    let files = [
        base.join("Marvel_PCD3D_SM6.upipelinecache"),
        base.join("pso_compile_cache_info.json"),
        base.join("Config")
            .join("Windows")
            .join("MachinePSOConfig.ini"),
    ];

    for f in &files {
        if f.exists() {
            fs::remove_file(f).map_err(|e| format!("Failed to delete {}: {}", f.display(), e))?;
            cleared += 1;
        }
    }

    let temp_dir = base.join("CollectedPSOs").join("Temp");
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)
            .map_err(|e| format!("Failed to remove {}: {}", temp_dir.display(), e))?;
        cleared += 1;
    }

    if cleared == 0 {
        Ok("Shader cache already clean".to_string())
    } else {
        Ok(format!(
            "Cleared {} shader cache item{}",
            cleared,
            if cleared != 1 { "s" } else { "" }
        ))
    }
}

#[tauri::command]
pub(crate) fn clear_shader_cache(state: State<'_, SettingsState>) -> Result<String, String> {
    let game_root = state.lock().ok().and_then(|s| s.game_path.clone());
    clear_shader_cache_files(game_root.as_deref())
}
