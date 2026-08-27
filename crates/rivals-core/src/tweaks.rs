//! Shared tweak engine consumed by pak_tweaks (mod Engine.ini/DeviceProfiles.ini) and game_user_settings (GameUserSettings.ini).

pub mod catalogue;
pub mod detect;
pub mod parser;

pub use catalogue::{TweakDefinition, TweakKind, TweakSetting, TweakState};
pub use detect::detect_active_tweaks_unscoped;

/// Detect which tweaks are active in flat key=value content with no section headers.
/// Used for pak INI content where section structure has already been collapsed.
pub fn detect_tweaks_unscoped(content: &str) -> Vec<TweakState> {
    let entries = catalogue::tweak_catalogue();
    detect_active_tweaks_unscoped(content, &entries)
}
