//! Unreal shader cache cleanup. Recommended after applying any rendering-related tweaks

use std::fs;

fn clear_shader_cache_files() -> Result<String, String> {
    let base = dirs::data_local_dir()
        .map(|p| p.join("Marvel").join("Saved"))
        .ok_or_else(|| "Could not determine AppData path.".to_string())?;
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
pub(crate) fn clear_shader_cache() -> Result<String, String> {
    clear_shader_cache_files()
}
