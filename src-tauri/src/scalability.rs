mod engine;
pub(crate) mod tweaks;

use std::{fs, path::Path};

pub(crate) use tweaks::{TweakDefinition, TweakSetting, TweakState};

const CONFIG_PATH: &str = "Marvel\\Saved\\Config\\Windows\\Scalability.ini";

pub(crate) fn get_scalability_path() -> Result<String, String> {
    dirs::data_local_dir()
        .map(|base| base.join(CONFIG_PATH))
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| "Could not determine AppData path.".to_string())
}

pub(crate) fn read_scalability(path: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| e.to_string())
}

pub(crate) fn write_scalability(path: &str, content: &str) -> Result<(), String> {
    let p = Path::new(path);
    if let Ok(meta) = fs::metadata(p) {
        if meta.permissions().readonly() {
            return Err("Scalability.ini is read-only. Remove the read-only attribute and try again.".to_string());
        }
    }
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(p, content).map_err(|e| e.to_string())
}

/// Return the full tweak catalogue.
pub(crate) fn get_tweak_definitions() -> Vec<TweakDefinition> {
    tweaks::tweak_catalogue()
}

/// Detect which tweaks are active in INI content.
pub(crate) fn detect_tweaks(content: &str) -> Vec<TweakState> {
    let catalogue = tweaks::tweak_catalogue();
    engine::detect_active_tweaks(content, &catalogue)
}

/// Apply tweak settings to INI content.
pub(crate) fn apply_tweaks(content: &str, settings: &[TweakSetting]) -> String {
    let catalogue = tweaks::tweak_catalogue();
    engine::apply_tweaks(content, &catalogue, settings)
}

pub(crate) fn clear_shader_cache() -> Result<String, String> {
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
