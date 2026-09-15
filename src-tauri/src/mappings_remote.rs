//! Lists and downloads .usmap mappings files published by the rivals-depot repository.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::settings::SettingsState;

const INDEX_URL: &str =
    "https://api.github.com/repos/SpaceDepot/rivals-depot/contents/usmap?ref=main";
const RAW_BASE: &str = "https://raw.githubusercontent.com/SpaceDepot/rivals-depot/main/usmap/";
const USER_AGENT: &str = "rivals-toolkit";
const TIMEOUT: Duration = Duration::from_secs(60);
/// Well above the largest published file, which is a little over a megabyte. A mappings file that
/// runs to hundreds of megabytes is a wrong URL, not a mappings file.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// The fields of a GitHub contents entry this needs. The response carries a dozen more.
#[derive(Deserialize)]
struct ContentEntry {
    name: String,
    size: u64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Serialize)]
pub(crate) struct RemoteMapping {
    pub name: String,
    pub size: u64,
    /// The engine changelist the build was cut from, parsed out of the file name. Names sort
    /// chronologically by this and not by anything else, so the newest one is the largest.
    pub changelist: u64,
    /// The season the name spells out, for picking one without reading the whole string.
    pub label: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .new_agent()
}

/// `5.3.2-3848703+++depot_marvel+S10.0_release-Marvel.usmap` -> `3848703`.
fn changelist_of(name: &str) -> u64 {
    name.split("+++")
        .next()
        .and_then(|head| head.rsplit(['-', '_']).next())
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

/// The same name -> `S10.0`. Falls back to the whole name when it is shaped differently.
fn label_of(name: &str) -> String {
    let Some(tail) = name.split("+++").nth(1) else {
        return name.trim_end_matches(".usmap").to_string();
    };
    tail.split('+')
        .nth(1)
        .and_then(|season| season.split('_').next())
        .map(str::to_string)
        .unwrap_or_else(|| name.trim_end_matches(".usmap").to_string())
}

/// Reject anything that is not a bare file name, so a name off the wire cannot be talked into
/// writing outside the mappings folder or into a URL of its own.
fn safe_name(name: &str) -> Result<&str, String> {
    let ok = name.ends_with(".usmap")
        && !name.is_empty()
        && name.len() <= 255
        && !name.contains(['/', '\\', ':', '?', '#'])
        && !name.starts_with('.');
    if ok {
        Ok(name)
    } else {
        Err(format!("{name} is not a mappings file name"))
    }
}

pub(crate) fn mappings_dir() -> Result<PathBuf, String> {
    let dir = dirs::config_dir()
        .ok_or_else(|| "no config directory on this system".to_string())?
        .join("rivals-toolkit")
        .join("mappings");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Newest first. The listing arrives in name order, which only happens to be chronological while
/// every changelist has the same number of digits, so it is sorted on the number itself.
fn fetch_index() -> Result<Vec<RemoteMapping>, String> {
    let mut response = agent()
        .get(INDEX_URL)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("could not reach the mappings repository: {e}"))?;
    let entries: Vec<ContentEntry> = response
        .body_mut()
        .read_json()
        .map_err(|e| format!("could not read the mappings listing: {e}"))?;

    let mut mappings: Vec<RemoteMapping> = entries
        .into_iter()
        .filter(|e| e.kind == "file" && e.name.ends_with(".usmap"))
        .map(|e| RemoteMapping {
            changelist: changelist_of(&e.name),
            label: label_of(&e.name),
            name: e.name,
            size: e.size,
        })
        .collect();
    mappings.sort_by(|a, b| b.changelist.cmp(&a.changelist).then(a.name.cmp(&b.name)));
    Ok(mappings)
}

fn fetch_file(name: &str) -> Result<Vec<u8>, String> {
    let mut response = agent()
        .get(format!("{RAW_BASE}{name}"))
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("could not download {name}: {e}"))?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not read {name}: {e}"))?;
    Ok(bytes)
}

#[tauri::command]
pub(crate) async fn list_remote_mappings() -> Result<Vec<RemoteMapping>, String> {
    tauri::async_runtime::spawn_blocking(fetch_index)
        .await
        .map_err(|e| e.to_string())?
}

/// Download one published mappings file and make it the one in use.
///
/// Only the file name crosses the boundary; the URL is built here, so the command cannot be
/// pointed at an arbitrary host.
#[tauri::command]
pub(crate) async fn download_remote_mapping(
    state: State<'_, SettingsState>,
    name: String,
) -> Result<String, String> {
    let path = tauri::async_runtime::spawn_blocking(move || {
        let name = safe_name(&name)?;
        let dest = mappings_dir()?.join(name);
        let bytes = fetch_file(name)?;

        // Parse before it replaces anything, so a truncated or wrong-repo download cannot leave
        // the app pointed at a file it will fail on every asset read.
        rivals_core::mappings::parse_check(&bytes)
            .map_err(|e| format!("{name} is not a usable mappings file: {e}"))?;

        // Written beside the target and renamed, so an interrupted download cannot leave a
        // half-file under a name that looks complete.
        let temp = dest.with_extension("usmap.part");
        std::fs::write(&temp, &bytes).map_err(|e| format!("could not write {name}: {e}"))?;
        std::fs::rename(&temp, &dest).map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            format!("could not save {name}: {e}")
        })?;
        Ok::<_, String>(dest.display().to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.usmap_path = Some(path.clone());
    guard.save()?;
    Ok(path)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    const REAL: &str = "5.3.2-3848703+++depot_marvel+S10.0_release-Marvel.usmap";

    #[test]
    fn a_published_name_gives_up_its_changelist_and_season() {
        assert_eq!(changelist_of(REAL), 3_848_703);
        assert_eq!(label_of(REAL), "S10.0");
        assert_eq!(
            changelist_of("5.3.2-1043850+++depot_marvel+Gamescom_202408-Marvel.usmap"),
            1_043_850
        );
        assert_eq!(
            label_of("5.3.2-1599834+++depot_marvel+S1_1_release-Marvel+PY.usmap"),
            "S1"
        );
        // One published file leads with a prefix rather than the engine version.
        assert_eq!(
            changelist_of("PY_5.3.2-1573788+++depot_marvel+S1_1_release-Marvel.usmap"),
            1_573_788
        );
    }

    #[test]
    fn a_name_that_is_shaped_differently_still_sorts_and_reads() {
        assert_eq!(changelist_of("mappings.usmap"), 0);
        assert_eq!(label_of("mappings.usmap"), "mappings");
    }

    /// Set `RIVALS_NETWORK_TESTS=1` to reach the real repository. Skipped otherwise, so the
    /// suite does not depend on being online.
    #[test]
    fn the_published_listing_parses_and_its_newest_file_is_a_real_mappings_file() {
        if std::env::var("RIVALS_NETWORK_TESTS").is_err() {
            return;
        }
        let listing = fetch_index().expect("fetch the listing");
        assert!(listing.len() > 20, "got {} entries", listing.len());
        assert!(
            listing.iter().all(|m| m.changelist > 0),
            "every published name should give up a changelist"
        );
        assert!(
            listing
                .windows(2)
                .all(|w| w[0].changelist >= w[1].changelist),
            "newest first"
        );

        let newest = &listing[0];
        let bytes = fetch_file(&newest.name).expect("download the newest");
        assert_eq!(bytes.len() as u64, newest.size, "downloaded the whole file");
        let (structs, enums) =
            rivals_core::mappings::parse_check(&bytes).expect("it should parse as mappings");
        assert!(
            structs > 1000 && enums > 100,
            "{structs} structs, {enums} enums"
        );
    }

    /// The name picks the file that is written and the URL that is fetched, so it has to be a
    /// bare name and nothing else.
    #[test]
    fn a_name_that_could_escape_the_mappings_folder_is_refused() {
        assert!(safe_name(REAL).is_ok());
        for bad in [
            "../../evil.usmap",
            "sub/dir.usmap",
            "sub\\dir.usmap",
            ".hidden.usmap",
            "notes.txt",
            "",
            "https://evil.test/x.usmap",
        ] {
            assert!(safe_name(bad).is_err(), "{bad} should be refused");
        }
    }
}
