use std::fs;

use crate::paths::launch_record_path;

const LAUNCHER_VALUE: &str = "6";
const SKIP_LAUNCHER_VALUE: &str = "0";
const SKIP_LAUNCHER_VALUE_ALT: &str = "-1";

/// Returns `true` if the launcher is set to be skipped (value `"0"`).
/// Missing file is treated as the default: launcher enabled.
pub(crate) fn get_skip_launcher(game_root: &str) -> Result<bool, String> {
    let path = launch_record_path(game_root);
    match fs::read_to_string(&path) {
        Ok(content) => match content.trim() {
            SKIP_LAUNCHER_VALUE | SKIP_LAUNCHER_VALUE_ALT => Ok(true),
            LAUNCHER_VALUE => Ok(false),
            other => Err(format!(
                "Unexpected launch_record value: \"{other}\". Expected \"{SKIP_LAUNCHER_VALUE}\", \"{SKIP_LAUNCHER_VALUE_ALT}\", or \"{LAUNCHER_VALUE}\"."
            )),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

/// Writes `"0"` (skip) or `"6"` (use launcher) to the `launch_record` file.
pub(crate) fn set_skip_launcher(game_root: &str, skip: bool) -> Result<(), String> {
    let path = launch_record_path(game_root);
    if let Ok(meta) = fs::metadata(&path)
        && meta.permissions().readonly()
    {
        return Err(
            "launch_record is read-only. Remove the read-only attribute and try again.".to_string(),
        );
    }
    let value = if skip {
        SKIP_LAUNCHER_VALUE
    } else {
        LAUNCHER_VALUE
    };
    fs::write(&path, value).map_err(|e| e.to_string())
}
