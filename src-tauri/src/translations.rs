//! Tauri commands that read a text's translations and give a mod its own wording in a language.

use serde::Serialize;
use tauri::State;

use rivals_core::localization::Localizer;
use rivals_core::text_mod::{self, TextChange, TextName, TextReport};
use rivals_core::{asset_edit, mappings};

use crate::asset_view::{configured_usmap, text_culture};
use crate::settings::SettingsState;

/// What the game shows for each of a string table's entries in the chosen language, in the order
/// given, `None` where it shows the source.
#[tauri::command]
pub(crate) async fn table_translations(
    state: State<'_, SettingsState>,
    game_root: String,
    namespace: String,
    sources: Vec<(String, String)>,
) -> Result<Vec<Option<String>>, String> {
    let culture = text_culture(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let localizer = Localizer::for_culture(&game_root, &culture)?;
        Ok(sources
            .iter()
            .map(|(key, source)| {
                localizer
                    .lookup(&namespace, key, Some(source))
                    .map(str::to_string)
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub(crate) struct Translation {
    pub culture: String,
    pub namespace: String,
    pub key: String,
    /// The game's own wording, when it translates the text.
    pub game: Option<String>,
    /// The wording the mod gives, where it differs from the game's.
    pub held: Option<String>,
}

fn resolve(game_root: &str, usmap: Option<&str>, text: &str) -> Result<(String, String), String> {
    let name = TextName::parse(text)?;
    let schema = match name {
        TextName::Table { .. } => mappings::resolve(None, usmap)
            .and_then(|path| mappings::load(&path))
            .ok(),
        TextName::Filed { .. } => None,
    };
    name.resolve(game_root, schema.as_deref())
}

/// A text's wording in the chosen language, as the game gives it and as the mod `mod_name` does.
#[tauri::command]
pub(crate) async fn text_translation(
    state: State<'_, SettingsState>,
    game_root: String,
    mod_name: String,
    text: String,
) -> Result<Translation, String> {
    let culture = text_culture(&state);
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let (namespace, key) = resolve(&game_root, usmap.as_deref(), &text)?;
        let (_, game) = text_mod::game_translations(&game_root, &culture)?;
        let game = game.get(&namespace, &key).map(|entry| entry.text.clone());
        let pak = asset_edit::mod_pak(&game_root, mod_name.trim())?;
        let held = text_mod::mod_translations(&pak, &culture)?
            .and_then(|(_, file)| file.get(&namespace, &key).map(|entry| entry.text.clone()))
            .filter(|held| Some(held) != game.as_ref());
        Ok(Translation {
            culture,
            namespace,
            key,
            game,
            held,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Gives a text the mod's own wording in the chosen language, or with no wording the game's back.
#[tauri::command]
pub(crate) async fn save_text_translation(
    state: State<'_, SettingsState>,
    game_root: String,
    mod_name: String,
    text: String,
    wording: Option<String>,
    overwrite: bool,
) -> Result<TextReport, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    let culture = text_culture(&state);
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let (namespace, key) = resolve(&game_root, usmap.as_deref(), &text)?;
        let pak = asset_edit::mod_pak(&game_root, mod_name.trim())?;
        let change = TextChange {
            culture,
            namespace,
            key,
            text: wording,
        };
        text_mod::apply_text(&game_root, &pak, &[change], overwrite, false)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The mods the toolkit keeps wordings for, by their names without case.
#[tauri::command]
pub(crate) fn text_edited_mods() -> Result<Vec<String>, String> {
    text_mod::recorded_mods()
}

/// Writes an installed mod's translations again over the game's current ones.
#[tauri::command]
pub(crate) async fn rebuild_mod_text(
    game_root: String,
    mod_pak: String,
) -> Result<TextReport, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let files = rivals_core::mod_ops::ModFiles::of(std::path::Path::new(&mod_pak))?;
        text_mod::rebuild_text(&game_root, &files.part("pak"), false, false)
    })
    .await
    .map_err(|e| e.to_string())?
}
