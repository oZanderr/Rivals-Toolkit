mod bypass;
pub(crate) mod conflicts;
mod folder;
pub(crate) mod profiles;
mod status;

pub(crate) use conflicts::ConflictReport;
pub(crate) use folder::{BulkOpResult, InstallResult};
pub(crate) use profiles::ProfileApplyResult;
pub(crate) use profiles::ProfileDiff;
pub(crate) use status::ModsStatus;

// Bypass files bundled at compile time.
static BYPASS_DSOUND: &[u8] = include_bytes!("../resources/bypass/dsound.dll");
static BYPASS_ASI: &[u8] =
    include_bytes!("../resources/bypass/plugins/MarvelRivalsUTOCSignatureBypass.asi");

/// Collect relative paths of mod-related files (.pak, .ucas, .utoc, and their
/// `.disabled` variants) under the given root directory. When `recursive` is
/// false, only direct children of `root` are scanned (matches UE's native
/// `~mods` load behavior).
pub(crate) fn walk_mod_files(root: &std::path::Path, recursive: bool) -> Vec<std::path::PathBuf> {
    let mut walker = walkdir::WalkDir::new(root);
    if !recursive {
        walker = walker.max_depth(1);
    }
    walker
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            let base = name.strip_suffix(".disabled").unwrap_or(&name);
            matches!(
                std::path::Path::new(base)
                    .extension()
                    .and_then(|x| x.to_str()),
                Some("pak" | "ucas" | "utoc")
            )
        })
        .filter_map(|e| e.path().strip_prefix(root).ok().map(|r| r.to_path_buf()))
        .collect()
}

/// Check whether a file exists and matches the expected bytes.
fn file_matches(path: &std::path::Path, expected: &[u8]) -> bool {
    std::fs::read(path)
        .map(|data| data == expected)
        .unwrap_or(false)
}

pub(crate) fn get_mods_status(game_root: &str, recursive: bool) -> ModsStatus {
    status::get_mods_status(game_root, recursive)
}

pub(crate) fn install_signature_bypass(game_root: &str) -> Result<String, String> {
    bypass::install_signature_bypass(game_root)
}

pub(crate) fn remove_signature_bypass(game_root: &str) -> Result<String, String> {
    bypass::remove_signature_bypass(game_root)
}

pub(crate) fn open_mods_folder(game_root: &str) -> Result<(), String> {
    folder::open_mods_folder(game_root)
}

pub(crate) fn toggle_mod_enabled(
    mods_folder: &str,
    full_name: &str,
    enabled: bool,
) -> Result<(), String> {
    folder::toggle_mod_enabled(mods_folder, full_name, enabled)
}

pub(crate) fn export_mods_archive(
    mods_folder: &str,
    dest_path: &str,
    recursive: bool,
) -> Result<String, String> {
    folder::export_mods_archive(mods_folder, dest_path, recursive)
}

pub(crate) fn delete_mod(mods_folder: &str, full_name: &str) -> Result<(), String> {
    folder::delete_mod(mods_folder, full_name)
}

pub(crate) fn toggle_mods_enabled(
    mods_folder: &str,
    names: &[String],
    enabled: bool,
) -> BulkOpResult {
    folder::toggle_mods_enabled(mods_folder, names, enabled)
}

pub(crate) fn delete_mods(mods_folder: &str, names: &[String]) -> BulkOpResult {
    folder::delete_mods(mods_folder, names)
}

pub(crate) fn install_mod(mods_folder: &str, source_path: &str) -> Result<InstallResult, String> {
    folder::install_mod(mods_folder, source_path)
}

pub(crate) fn rename_mod(
    mods_folder: &str,
    full_name: &str,
    new_base: &str,
) -> Result<String, String> {
    folder::rename_mod(mods_folder, full_name, new_base)
}

pub(crate) fn install_from_archive(
    mods_folder: &str,
    archive_path: &str,
) -> Result<Vec<InstallResult>, String> {
    folder::install_from_archive(mods_folder, archive_path)
}

pub(crate) fn check_conflicts(game_root: &str, recursive: bool) -> Result<ConflictReport, String> {
    conflicts::check_conflicts(game_root, recursive)
}
