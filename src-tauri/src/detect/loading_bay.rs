use std::path::PathBuf;

use super::registry::hkcu_str;

// Marvel Rivals game ID
const LOADING_BAY_GAME_ID: u32 = 31;

pub(super) fn find_loading_bay_install() -> Option<PathBuf> {
    let game_key = format!(r"Software\LoadingBay\LoadingBayInstaller\game\{LOADING_BAY_GAME_ID}");

    if let Some(raw) = hkcu_str(&game_key, "InstallPath") {
        let p = PathBuf::from(raw.trim().replace('/', "\\"));
        if p.exists() { return Some(p); }
    }

    let default_root = hkcu_str(r"Software\LoadingBay\LoadingBayInstaller\setting", "defaultGamePath")?;
    let p = PathBuf::from(default_root.trim().replace('/', "\\")).join("Marvel Rivals");
    p.exists().then_some(p)
}
