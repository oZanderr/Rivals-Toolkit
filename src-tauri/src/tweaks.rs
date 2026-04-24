//! Shared tweak engine consumed by both scalability (system Scalability.ini) and pak_tweaks (mod Engine.ini/DeviceProfiles.ini).

pub(crate) mod catalogue;
pub(crate) mod detect;
pub(crate) mod parser;
pub(crate) mod shader_cache;

pub(crate) use catalogue::{TweakDefinition, TweakKind, TweakLine, TweakSetting, TweakState};
pub(crate) use detect::detect_active_tweaks;

/// Detect which tweaks are active in arbitrary INI content using the bundled catalogue.
pub(crate) fn detect_tweaks(content: &str) -> Vec<TweakState> {
    let entries = catalogue::tweak_catalogue();
    detect_active_tweaks(content, &entries)
}
