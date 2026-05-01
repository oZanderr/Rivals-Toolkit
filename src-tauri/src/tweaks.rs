//! Shared tweak engine consumed by both scalability (system Scalability.ini) and pak_tweaks (mod Engine.ini/DeviceProfiles.ini).

pub(crate) mod catalogue;
pub(crate) mod detect;
pub(crate) mod parser;
pub(crate) mod profiles;
pub(crate) mod shader_cache;

pub(crate) use catalogue::{TweakDefinition, TweakKind, TweakLine, TweakSetting, TweakState};
pub(crate) use detect::{detect_active_tweaks, detect_active_tweaks_unscoped};

/// Detect which tweaks are active in arbitrary INI content using the bundled catalogue.
/// Section-scoped: matches keys only within their declared scalability_section.
pub(crate) fn detect_tweaks(content: &str) -> Vec<TweakState> {
    let entries = catalogue::tweak_catalogue();
    detect_active_tweaks(content, &entries)
}

/// Detect which tweaks are active in flat key=value content with no section headers.
/// Used for pak INI content where scalability_section doesn't apply.
pub(crate) fn detect_tweaks_unscoped(content: &str) -> Vec<TweakState> {
    let entries = catalogue::tweak_catalogue();
    detect_active_tweaks_unscoped(content, &entries)
}
