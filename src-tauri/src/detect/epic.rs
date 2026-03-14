use std::{fs, path::PathBuf};

use serde_json::Value;

use super::registry::hklm_str;

const MARVEL_EPIC_APP_NAME: &str = "MarvelRivals";

fn epic_data_path() -> Option<PathBuf> {
    let raw = hklm_str(r"SOFTWARE\Epic Games\EpicGamesLauncher", "AppDataPath")?;
    let p = PathBuf::from(raw.trim());
    p.exists().then_some(p)
}

/// Returns the install path and the Epic launcher URL to launch the game.
pub(super) fn find_epic_install() -> Option<(PathBuf, String)> {
    let data_dir = epic_data_path()
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\Epic\EpicGamesLauncher\Data"));

    fs::read_dir(data_dir.join("Manifests"))
        .ok()?
        .flatten()
        .find_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("item") {
                return None;
            }
            let content = fs::read_to_string(&path).ok()?;
            let json = serde_json::from_str::<Value>(&content).ok()?;

            let app_name = json["AppName"].as_str().unwrap_or_default();
            let display_name = json["DisplayName"].as_str().unwrap_or_default();

            if !app_name.eq_ignore_ascii_case(MARVEL_EPIC_APP_NAME)
                && !display_name.eq_ignore_ascii_case("Marvel Rivals")
            {
                return None;
            }

            // Epic manifests may use forward slashes on Windows
            let location = json["InstallLocation"].as_str()?;
            let p = PathBuf::from(location.replace('/', "\\"));
            if !p.exists() {
                return None;
            }

            // Build the proper launcher URL from catalog fields in the manifest.
            // Format: com.epicgames.launcher://apps/{ns}%3A{itemId}%3A{appName}?action=launch&silent=true
            let ns = json["CatalogNamespace"]
                .as_str()
                .filter(|s| !s.is_empty())?;
            let item_id = json["CatalogItemId"].as_str().filter(|s| !s.is_empty())?;
            let name = json["AppName"].as_str().filter(|s| !s.is_empty())?;
            let launch_url = format!(
                "com.epicgames.launcher://apps/{}%3A{}%3A{}?action=launch&silent=true",
                ns, item_id, name
            );

            Some((p, launch_url))
        })
}
