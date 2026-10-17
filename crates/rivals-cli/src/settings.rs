//! Reads the desktop app's `settings.json` so the CLI defaults match what the GUI is configured to do.

use std::path::PathBuf;

use rivals_core::pak::profile::OodleLevel;
use rivals_core::tweaks::TweakSetting;
use serde::Deserialize;

/// Only the fields the CLI honours. Unknown keys are ignored, so the app is free to add settings
/// without breaking a CLI built against an older schema.
#[derive(Debug, Default, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub game_path: Option<String>,
    #[serde(default = "yes")]
    pub recursive_mod_scan: bool,
    #[serde(default = "yes")]
    pub game_running_check_enabled: bool,
    #[serde(default)]
    pub usmap_path: Option<String>,
    /// A list of native object paths to name imports by, on top of the bundled one.
    #[serde(default)]
    pub extra_script_objects_path: Option<String>,
    /// The mod pak the desktop app last saved asset edits into.
    #[serde(default)]
    pub asset_mod_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_save_target: Option<rivals_core::asset_edit::SaveTarget>,
    /// The language the desktop app shows texts in.
    #[serde(default)]
    pub text_culture: Option<String>,
    /// Tweak presets saved from the desktop app, so `--preset` applies the same thing the GUI
    /// would.
    #[serde(default)]
    pub tweak_profiles: Vec<TweakProfile>,
    /// How hard Oodle works on the mods the desktop app writes. Read leniently: a value this build
    /// does not know is no level, rather than the whole file unread.
    #[serde(default, deserialize_with = "lenient")]
    pub mod_compression_level: Option<OodleLevel>,
    /// A log key and IV entered in the desktop app, in hex, in place of the bundled ones.
    #[serde(default, deserialize_with = "lenient")]
    pub log_key: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub log_iv: Option<String>,
}

/// The value when it reads as one, else nothing.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// One saved preset. The app stores timestamps alongside these; the CLI only needs what to write.
#[derive(Debug, Clone, Deserialize)]
pub struct TweakProfile {
    pub name: String,
    #[serde(default)]
    pub settings: Vec<TweakSetting>,
}

fn yes() -> bool {
    true
}

pub fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rivals-toolkit").join("settings.json"))
}

/// Load the app's settings, falling back to defaults when the file is absent or unreadable. A
/// missing settings file is normal (the CLI can run on a machine that never opened the app), so it
/// is not an error here; commands that actually need a game root report that themselves.
pub fn load() -> AppSettings {
    let Some(path) = settings_path() else {
        return AppSettings {
            recursive_mod_scan: true,
            game_running_check_enabled: true,
            ..AppSettings::default()
        };
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(AppSettings {
            recursive_mod_scan: true,
            game_running_check_enabled: true,
            ..AppSettings::default()
        })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The app's level reads by the name it stores, and a value no build knows leaves the rest of
    /// the file read.
    #[test]
    fn the_mod_compression_level_reads_leniently() {
        let read = |raw: &str| serde_json::from_str::<AppSettings>(raw).expect("settings read");
        assert_eq!(
            read(r#"{"mod_compression_level":"Optimal2"}"#).mod_compression_level,
            Some(OodleLevel::Optimal2)
        );
        let odd = read(r#"{"mod_compression_level":"Hyper","game_path":"C:/Game"}"#);
        assert_eq!(odd.mod_compression_level, None);
        assert_eq!(odd.game_path.as_deref(), Some("C:/Game"));
        assert_eq!(read("{}").mod_compression_level, None);
    }
}
