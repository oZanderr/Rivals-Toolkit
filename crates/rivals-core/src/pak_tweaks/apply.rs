//! Applies catalogue-driven edits and raw INI content saves to pak files in place.

use std::fs;
use std::path::Path;

use crate::pak::profile::strip_mount_prefix;

use super::cvars::{IniType, apply_edits_to_ini, sets_key};
use super::io::{inspect_pak_for_ini, with_unpacked_pak};
use super::{PakIniFileContent, PakIniTarget, PakTweakEdit};

/// One INI file out of a pak, tagged with the config layer it belongs to.
#[derive(Debug, Clone)]
pub(super) struct LayerFile {
    pub target: PakIniTarget,
    pub content: String,
}

impl LayerFile {
    fn ini_type(&self) -> IniType {
        if self.target.is_engine() {
            IniType::Engine
        } else {
            IniType::DeviceProfiles
        }
    }
}

/// Drive a whole pak's worth of files the way `apply_pak_tweaks` does, for the tests that assert
/// across every layer at once.
#[cfg(test)]
pub(super) fn apply_edits_to_layers(files: &mut [LayerFile], edits: &[PakTweakEdit]) {
    for file in files.iter_mut() {
        apply_edits_to_file(file, edits);
    }
}

/// Apply `edits` to one INI file, reporting whether they changed it.
///
/// Callers run this over every file a pak ships. A value is written to all of them, so opening any
/// one shows the tweak and no file can shadow another with a stale copy. A removal clears the key
/// from every file and every section that sets it, since whichever copy survives is the one the
/// game ends up reading. Engine-section settings are not console variables, so they stay inside
/// engine files.
///
/// One file at a time is the unit because they are independent: a pak of large configs never has
/// to hold more than the file being edited.
pub(super) fn apply_edits_to_file(file: &mut LayerFile, edits: &[PakTweakEdit]) -> bool {
    let (engine_section_edits, plain_edits): (Vec<PakTweakEdit>, Vec<PakTweakEdit>) = edits
        .iter()
        .cloned()
        .partition(|e| e.engine_section.is_some());

    let plain = apply_group(file, &plain_edits, |_| true);
    let engine = apply_group(file, &engine_section_edits, PakIniTarget::is_engine);
    plain || engine
}

fn apply_group(
    file: &mut LayerFile,
    edits: &[PakTweakEdit],
    eligible: fn(PakIniTarget) -> bool,
) -> bool {
    if edits.is_empty() || !eligible(file.target) {
        return false;
    }
    // A removal has nothing to do in a file that never sets the key, and skipping it keeps that
    // file byte-for-byte instead of reformatting it for nothing. The question is wider than what
    // the file reports as state: a copy the client never reads still has to go.
    let applicable: Vec<PakTweakEdit> = edits
        .iter()
        .filter(|e| e.value.is_some() || sets_key(&file.content, &e.key))
        .cloned()
        .collect();
    if applicable.is_empty() {
        return false;
    }
    let next = apply_edits_to_ini(&file.content, &applicable, file.ini_type());
    if next == file.content {
        return false;
    }
    file.content = next;
    true
}

/// Apply catalogue-driven edits to a pak INI files and repack in place.
pub fn apply_pak_tweaks(pak_path: &str, edits: &[PakTweakEdit]) -> Result<String, String> {
    let pak = Path::new(pak_path);
    let info = inspect_pak_for_ini(pak)?
        .ok_or_else(|| "No INI config files found in this pak.".to_string())?;
    let pak_name = info.pak_name.clone();
    let edit_count = edits.len();
    let layers: Vec<(PakIniTarget, String)> = info
        .layers()
        .into_iter()
        .map(|(target, entry)| (target, entry.to_string()))
        .collect();

    // One file at a time. A config mod can ship hundreds of megabytes of INI, and holding every
    // file plus a copy of each to compare against cost well over a gigabyte for a single tweak.
    let mut files_changed = 0usize;
    with_unpacked_pak(pak, |temp_dir| {
        for (target, entry) in &layers {
            let path = temp_dir.join(strip_mount_prefix(entry));
            let content = fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read extracted INI {}: {}", path.display(), e))?;
            let mut file = LayerFile {
                target: *target,
                content,
            };
            if apply_edits_to_file(&mut file, edits) {
                fs::write(&path, &file.content).map_err(|e| {
                    format!("Failed to write modified INI {}: {}", path.display(), e)
                })?;
                files_changed += 1;
            }
        }
        Ok(())
    })?;

    // What was asked for and what it did are different numbers. A preset names every tweak in the
    // catalogue whether or not the pak needs it, so re-applying one used to report the full count
    // and read as though none of it had ever been applied.
    if files_changed == 0 {
        return Ok(format!("{pak_name} already matches: nothing to change"));
    }
    let label = if edit_count == 1 { "change" } else { "changes" };
    let files = if files_changed == 1 { "file" } else { "files" };
    Ok(format!(
        "Applied {edit_count} {label} to {pak_name} ({files_changed} {files} rewritten)"
    ))
}

/// Replace raw INI file contents in a pak and repack in place. `files` writes are
/// applied first (creating parent dirs for brand-new entries), then `deletes` are
/// removed from the temp tree; repack picks up whatever remains.
pub fn save_pak_ini(
    pak_path: &str,
    files: Vec<PakIniFileContent>,
    deletes: Vec<String>,
) -> Result<String, String> {
    let pak = Path::new(pak_path);
    let change_count = files.len() + deletes.len();

    with_unpacked_pak(pak, |temp_dir| {
        for file in &files {
            let rel = strip_mount_prefix(&file.entry);
            let dest = temp_dir.join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
            }
            match &file.staged_path {
                // Staged text is moved rather than read back through memory, so a file that was
                // too large to hand over in one piece is never held whole here either.
                Some(staged) => {
                    fs::copy(staged, &dest).map_err(|e| {
                        format!("Failed to write {} from {staged}: {}", dest.display(), e)
                    })?;
                    let _ = fs::remove_file(staged);
                }
                None => fs::write(&dest, &file.content)
                    .map_err(|e| format!("Failed to write {}: {}", dest.display(), e))?,
            }
        }
        for entry in &deletes {
            let rel = strip_mount_prefix(entry);
            let dest = temp_dir.join(rel);
            match fs::remove_file(&dest) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("Failed to delete {}: {}", dest.display(), e)),
            }
        }
        Ok(())
    })?;

    let pak_name = pak
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    Ok(format!("Saved {} change(s) to {}", change_count, pak_name))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    //! Full-pak verification: build the five INI files a config mod can ship, seed the same
    //! CVar into every file and every section that is live at runtime, then drive the real
    //! `apply_edits_to_layers` and assert nothing survives that should not.
    //!
    //! The bug this suite exists for: a leftover copy in `BaseDeviceProfiles.ini` or in
    //! `[WindowsClient DeviceProfile]` kept applying after the toggle reported the fix as done.

    use super::*;
    use crate::pak_tweaks::cvars::parse_console_vars;
    use crate::pak_tweaks::{PakCvar, edits_for_settings, edits_for_tweak};
    use crate::tweaks::catalogue::{TweakDefinition, TweakKind, tweak_catalogue};
    use crate::tweaks::{TweakSetting, TweakState, detect_tweaks_unscoped};

    /// The editor names this field as the struct spells it. Getting that wrong would not fail
    /// loudly: the field would read as absent and the save would write an empty file over the
    /// config the user had just edited.
    #[test]
    fn a_staged_path_arrives_under_the_name_the_editor_sends() {
        let from_editor = r#"{"entry":"Marvel/Config/DefaultEngine.ini","content":"","staged_path":"C:/tmp/x.ini"}"#;
        let parsed: PakIniFileContent =
            serde_json::from_str(from_editor).expect("deserialize a staged file");
        assert_eq!(parsed.staged_path.as_deref(), Some("C:/tmp/x.ini"));

        // A file with no staged text still parses, which is what every inline save sends.
        let inline = r#"{"entry":"a.ini","content":"x=1"}"#;
        let parsed: PakIniFileContent =
            serde_json::from_str(inline).expect("deserialize an inline file");
        assert_eq!(parsed.staged_path, None);
        assert_eq!(parsed.content, "x=1");
    }

    /// Text handed over as a staged file has to land in the pak exactly as inline text would,
    /// or a large config would save as something subtly different from a small one.
    #[test]
    fn a_staged_file_saves_the_same_bytes_as_an_inline_one() {
        let dir = std::env::temp_dir().join(format!(
            "rivals-ini-staged-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        let entry = "Marvel/Config/DefaultEngine.ini";
        let body = "[ConsoleVariables]\r\nr.One=1\r\nr.Two=2";

        let saved = |files: Vec<PakIniFileContent>, name: &str| -> Vec<u8> {
            let pak = dir.join(name);
            crate::pak_tweaks::io::create_empty_pak(&pak).expect("pak");
            save_pak_ini(&pak.to_string_lossy(), files, Vec::new()).expect("save");
            let out = dir.join(format!("{name}-unpacked"));
            std::fs::create_dir_all(&out).expect("out");
            crate::pak_tweaks::io::unpack_to_dir(&pak, &out).expect("unpack");
            std::fs::read(out.join(entry)).expect("read back")
        };

        let inline = saved(
            vec![PakIniFileContent {
                entry: entry.to_string(),
                content: body.to_string(),
                staged_path: None,
            }],
            "inline.pak",
        );

        let staged_file = dir.join("staged.txt");
        std::fs::write(&staged_file, body).expect("stage");
        let staged = saved(
            vec![PakIniFileContent {
                entry: entry.to_string(),
                content: String::new(),
                staged_path: Some(staged_file.to_string_lossy().into_owned()),
            }],
            "staged.pak",
        );

        assert_eq!(inline, body.as_bytes(), "inline content round trips");
        assert_eq!(staged, inline, "staged content is the same bytes as inline");
        assert!(
            !staged_file.exists(),
            "the staging file is consumed by the save"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Windows device profiles the shipping client can run. `Windows` is the active one and the
    /// rest inherit from it, so a CVar parked in any of them still reaches the game.
    const DP_SECTIONS: [&str; 4] = [
        "[Windows DeviceProfile]",
        "[WindowsClient DeviceProfile]",
        "[WindowsNoEditor DeviceProfile]",
        "[WindowsServer DeviceProfile]",
    ];

    /// Engine sections a config mod scatters console variables across.
    const ENGINE_SECTIONS: [&str; 2] = ["[ConsoleVariables]", "[SystemSettings]"];

    fn layer(target: PakIniTarget, content: String) -> LayerFile {
        LayerFile { target, content }
    }

    fn plain(edits: &[PakTweakEdit]) -> Vec<&PakTweakEdit> {
        edits
            .iter()
            .filter(|e| e.engine_section.is_none())
            .collect()
    }

    /// Engine file holding every key the edits touch, duplicated across both console-variable
    /// sections plus each explicit engine section.
    fn seeded_engine(edits: &[PakTweakEdit], value: &str) -> String {
        let mut out = String::new();
        for section in ENGINE_SECTIONS {
            out.push_str(section);
            out.push_str("\r\n");
            for edit in plain(edits) {
                out.push_str(&format!("{}={}\r\n", edit.key, value));
            }
            out.push_str("\r\n");
        }
        let mut sections: Vec<&str> = Vec::new();
        for section in edits.iter().filter_map(|e| e.engine_section.as_deref()) {
            if !sections.contains(&section) {
                sections.push(section);
            }
        }
        for section in sections {
            out.push_str(&format!("[{section}]\r\n"));
            for edit in edits
                .iter()
                .filter(|e| e.engine_section.as_deref() == Some(section))
            {
                out.push_str(&format!("{}={}\r\n", edit.key, value));
            }
            out.push_str("\r\n");
        }
        out
    }

    /// DeviceProfiles file holding every plain key under every Windows profile.
    fn seeded_dp(edits: &[PakTweakEdit], value: &str) -> String {
        let mut out = String::new();
        for section in DP_SECTIONS {
            out.push_str(section);
            out.push_str("\r\nDeviceType=Windows\r\n");
            for edit in plain(edits) {
                out.push_str(&format!("+CVars={}={}\r\n", edit.key, value));
            }
            out.push_str("\r\n");
        }
        out
    }

    /// The five-file pak the report came from, with `value` already set everywhere.
    fn seeded_pak(edits: &[PakTweakEdit], value: &str) -> Vec<LayerFile> {
        vec![
            layer(PakIniTarget::BaseEngine, seeded_engine(edits, value)),
            layer(PakIniTarget::Engine, seeded_engine(edits, value)),
            layer(PakIniTarget::WindowsEngine, seeded_engine(edits, value)),
            layer(PakIniTarget::BaseDeviceProfiles, seeded_dp(edits, value)),
            layer(PakIniTarget::DeviceProfiles, seeded_dp(edits, value)),
        ]
    }

    /// The same five files with section headers but no CVars.
    fn empty_pak() -> Vec<LayerFile> {
        let engine = format!(
            "{}\r\n\r\n{}\r\n\r\n",
            ENGINE_SECTIONS[0], ENGINE_SECTIONS[1]
        );
        let dp = format!("{}\r\nDeviceType=Windows\r\n\r\n", DP_SECTIONS[0]);
        vec![
            layer(PakIniTarget::BaseEngine, engine.clone()),
            layer(PakIniTarget::Engine, engine.clone()),
            layer(PakIniTarget::WindowsEngine, engine),
            layer(PakIniTarget::BaseDeviceProfiles, dp.clone()),
            layer(PakIniTarget::DeviceProfiles, dp),
        ]
    }

    /// Every assignment of `key` anywhere in the pak, in any section, comment lines excluded.
    /// Deliberately section-blind: a survivor in a section the editor does not know about is
    /// exactly the failure being tested for.
    fn key_hits(files: &[LayerFile], key: &str) -> Vec<(PakIniTarget, String)> {
        let key_lower = key.to_ascii_lowercase();
        let mut hits = Vec::new();
        for file in files {
            for line in file.content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with(';') {
                    continue;
                }
                let inner = trimmed
                    .strip_prefix("+CVars=")
                    .or_else(|| trimmed.strip_prefix("+cvars="))
                    .unwrap_or(trimmed);
                if let Some((k, v)) = inner.split_once('=')
                    && k.trim().to_ascii_lowercase() == key_lower
                {
                    hits.push((file.target, v.trim().to_string()));
                }
            }
        }
        hits
    }

    fn hits_in(files: &[LayerFile], target: PakIniTarget, key: &str) -> usize {
        key_hits(files, key)
            .iter()
            .filter(|(t, _)| *t == target)
            .count()
    }

    /// The flat key=value view `read_pak_cvars` builds, lowest priority first so the last
    /// layer wins, fed to the detector exactly as `detect_pak_tweaks` does.
    fn merged(files: &[LayerFile]) -> String {
        let mut merged: Vec<PakCvar> = Vec::new();
        for file in files {
            for var in parse_console_vars(&file.content, file.target.source_label()) {
                let key_lower = var.key.to_ascii_lowercase();
                merged.retain(|v| v.key.to_ascii_lowercase() != key_lower);
                merged.push(var);
            }
        }
        merged
            .iter()
            .map(|v| format!("{}={}", v.key, v.value))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn is_active(files: &[LayerFile], id: &str) -> bool {
        detect_tweaks_unscoped(&merged(files))
            .into_iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("no detected state for {id}"))
            .active
    }

    /// ON-state edits built by the same translation both front ends call.
    fn edits_on(def: &TweakDefinition) -> Vec<PakTweakEdit> {
        edits_for_tweak(def, true, slider_target(def).as_deref())
            .unwrap_or_else(|e| panic!("{} failed to translate ON: {e}", def.id))
    }

    /// A slider only reads back as enabled away from its default, so an ON request picks the far
    /// end of the range. Everything else takes its values from the catalogue.
    fn slider_target(def: &TweakDefinition) -> Option<String> {
        match &def.kind {
            TweakKind::Slider {
                min,
                max,
                default_value,
                ..
            } => {
                let away = if (default_value - min).abs() < f64::EPSILON {
                    max
                } else {
                    min
                };
                Some(format!("{away}"))
            }
            _ => None,
        }
    }

    /// OFF-state edits, or `None` for tweaks that only remove and cannot be restored.
    fn edits_off(def: &TweakDefinition) -> Option<Vec<PakTweakEdit>> {
        edits_for_tweak(def, false, None).ok()
    }

    /// A set must reach every file that already had the key, and a removal must clear the key
    /// out of every file and every section.
    fn assert_edit_landed(files: &[LayerFile], edit: &PakTweakEdit, what: &str) {
        let hits = key_hits(files, &edit.key);
        match edit.value.as_deref() {
            None => assert!(
                hits.is_empty(),
                "{what}: {} survives in {:?}",
                edit.key,
                hits
            ),
            Some(expected) => {
                assert!(!hits.is_empty(), "{what}: {} was never written", edit.key);
                for (target, value) in &hits {
                    assert_eq!(
                        value, expected,
                        "{what}: {} in {target:?} should be {expected}",
                        edit.key
                    );
                }
            }
        }
    }

    fn edit(key: &str, value: Option<&str>) -> PakTweakEdit {
        PakTweakEdit {
            key: key.into(),
            value: value.map(str::to_string),
            engine_section: None,
        }
    }

    // ── The reported bug ──────────────────────────────────────────────

    #[test]
    fn mipmap_fix_clears_every_file_and_every_windows_profile() {
        let def = tweak_catalogue()
            .into_iter()
            .find(|t| t.id == "fix_mipmap_bias")
            .expect("fix_mipmap_bias");
        let edits = edits_on(&def);
        let mut files = seeded_pak(&edits, "15");

        assert!(
            !is_active(&files, "fix_mipmap_bias"),
            "the seeded pak must read as not-yet-fixed"
        );

        apply_edits_to_layers(&mut files, &edits);

        assert!(
            key_hits(&files, "r.MipMapLODBias").is_empty(),
            "every copy must go, including BaseDeviceProfiles and the inherited profiles:\n{:#?}",
            key_hits(&files, "r.MipMapLODBias")
        );
        assert!(is_active(&files, "fix_mipmap_bias"));
    }

    #[test]
    fn a_copy_only_in_base_device_profiles_is_seen_and_removed() {
        let mut files = empty_pak();
        files[3].content = "[Windows DeviceProfile]\r\n+CVars=r.MipMapLODBias=15\r\n".into();

        assert!(
            !is_active(&files, "fix_mipmap_bias"),
            "BaseDeviceProfiles must be part of the merged read"
        );

        let def = tweak_catalogue()
            .into_iter()
            .find(|t| t.id == "fix_mipmap_bias")
            .expect("fix_mipmap_bias");
        apply_edits_to_layers(&mut files, &edits_on(&def));

        assert!(key_hits(&files, "r.MipMapLODBias").is_empty());
    }

    #[test]
    fn a_copy_only_in_an_inherited_profile_is_seen_and_removed() {
        let mut files = empty_pak();
        files[4].content = concat!(
            "[Windows DeviceProfile]\r\n",
            "DeviceType=Windows\r\n\r\n",
            "[WindowsClient DeviceProfile]\r\n",
            "BaseProfileName=Windows\r\n",
            "+CVars=r.MipMapLODBias=15\r\n"
        )
        .into();

        assert!(!is_active(&files, "fix_mipmap_bias"));

        let def = tweak_catalogue()
            .into_iter()
            .find(|t| t.id == "fix_mipmap_bias")
            .expect("fix_mipmap_bias");
        apply_edits_to_layers(&mut files, &edits_on(&def));

        assert!(key_hits(&files, "r.MipMapLODBias").is_empty());
    }

    // ── Force Default Material, both directions ───────────────────────

    #[test]
    fn force_default_material_off_clears_every_instance_then_on_restores_one_per_file() {
        let def = tweak_catalogue()
            .into_iter()
            .find(|t| t.id == "force_default_material")
            .expect("force_default_material");
        let key = "r.debug.ForceDefaultMtl";

        let on = edits_on(&def);
        let mut files = seeded_pak(&on, "1");
        assert_eq!(
            key_hits(&files, key).len(),
            14,
            "seed: 2 engine sections x 3 engine files + 4 profiles x 2 device profile files"
        );
        assert!(is_active(&files, "force_default_material"));

        let off = edits_off(&def).expect("force_default_material can be turned off");
        apply_edits_to_layers(&mut files, &off);
        assert!(
            key_hits(&files, key).is_empty(),
            "turning it off must remove every copy:\n{:#?}",
            key_hits(&files, key)
        );
        assert!(!is_active(&files, "force_default_material"));

        apply_edits_to_layers(&mut files, &on);
        let hits = key_hits(&files, key);
        assert_eq!(
            hits,
            vec![
                (PakIniTarget::BaseEngine, "1".into()),
                (PakIniTarget::Engine, "1".into()),
                (PakIniTarget::WindowsEngine, "1".into()),
                (PakIniTarget::BaseDeviceProfiles, "1".into()),
                (PakIniTarget::DeviceProfiles, "1".into()),
            ],
            "turning it back on writes one line per file, so every file shows the tweak"
        );
        assert!(
            files[4]
                .content
                .contains("+CVars=r.debug.ForceDefaultMtl=1"),
            "device profile CVars need the +CVars= prefix:\n{}",
            files[4].content
        );
        assert!(is_active(&files, "force_default_material"));
    }

    /// A stray copy in a file the merged view does not speak for is still cleared by a removal.
    ///
    /// Detection answers what the game ends up seeing, so the highest-priority layer decides what
    /// a key reads as for the whole pak. A hand-written config mod can leave the key set in a
    /// lower layer while a higher one turns it off, and the row then reports the state the caller
    /// already wanted. Treating "already in that state" as "nothing to write" leaves those copies
    /// in place, which is why the preset queues its tweaks whatever the pak reports.
    #[test]
    fn a_removal_clears_copies_the_merged_view_does_not_report() {
        let def = tweak_catalogue()
            .into_iter()
            .find(|t| t.id == "force_default_material")
            .expect("force_default_material");
        let key = "r.debug.ForceDefaultMtl";

        let dp = |value: &str| {
            format!(
                "{}\r\nDeviceType=Windows\r\n+CVars={key}={value}\r\n\r\n",
                DP_SECTIONS[0]
            )
        };
        let mut files = vec![
            layer(
                PakIniTarget::BaseEngine,
                format!("{}\r\n{key}=1\r\n\r\n", ENGINE_SECTIONS[0]),
            ),
            layer(
                PakIniTarget::Engine,
                format!("{}\r\n\r\n", ENGINE_SECTIONS[0]),
            ),
            layer(PakIniTarget::BaseDeviceProfiles, dp("1")),
            layer(PakIniTarget::DeviceProfiles, dp("0")),
        ];

        assert_eq!(key_hits(&files, key).len(), 3, "seeded into three files");
        assert!(
            !is_active(&files, "force_default_material"),
            "the highest-priority layer turns it off, so the pak reads as off"
        );

        // The state the caller wants is the state the pak already reports, so anything comparing
        // the two decides there is nothing to do. Applying the removal anyway is what reaches the
        // two files still carrying it.
        let off = edits_off(&def).expect("force_default_material can be turned off");
        for file in files.iter_mut() {
            apply_edits_to_file(file, &off);
        }
        assert!(
            key_hits(&files, key).is_empty(),
            "every copy has to go, including the ones the merged view never reported: {:#?}",
            key_hits(&files, key)
        );
    }

    // ── Whole-catalogue sweeps ────────────────────────────────────────

    #[test]
    fn every_tweak_turned_on_reaches_every_instance() {
        for def in tweak_catalogue() {
            let edits = edits_on(&def);
            if edits.is_empty() {
                continue;
            }
            let mut files = seeded_pak(&edits, "999");
            apply_edits_to_layers(&mut files, &edits);
            for edit in &edits {
                assert_edit_landed(&files, edit, &format!("{} ON", def.id));
            }
            assert!(
                is_active(&files, &def.id),
                "{} should read active after being turned on",
                def.id
            );
        }
    }

    #[test]
    fn every_tweak_turned_off_reaches_every_instance() {
        for def in tweak_catalogue() {
            let on = edits_on(&def);
            let Some(off) = edits_off(&def) else {
                continue;
            };
            if off.is_empty() {
                continue;
            }
            let mut files = seeded_pak(&on, "999");
            apply_edits_to_layers(&mut files, &off);
            for edit in &off {
                assert_edit_landed(&files, edit, &format!("{} OFF", def.id));
            }
            assert!(
                !is_active(&files, &def.id),
                "{} should read inactive after being turned off",
                def.id
            );
        }
    }

    #[test]
    fn every_tweak_turned_on_from_scratch_writes_to_every_managed_file() {
        for def in tweak_catalogue() {
            let edits = edits_on(&def);
            if edits.is_empty() {
                continue;
            }
            let mut files = empty_pak();
            apply_edits_to_layers(&mut files, &edits);

            for edit in &edits {
                let hits = key_hits(&files, &edit.key);
                match edit.value.as_deref() {
                    None => assert!(
                        hits.is_empty(),
                        "{}: a removal must not inject {}",
                        def.id,
                        edit.key
                    ),
                    Some(expected) => {
                        // An engine-section setting is not a console variable, so a device
                        // profile cannot hold it.
                        let want: Vec<(PakIniTarget, String)> = PakIniTarget::ALL
                            .iter()
                            .filter(|t| edit.engine_section.is_none() || t.is_engine())
                            .map(|t| (*t, expected.to_string()))
                            .collect();
                        assert_eq!(
                            hits, want,
                            "{}: {} should be written to every file that can hold it",
                            def.id, edit.key
                        );
                    }
                }
            }
            assert!(is_active(&files, &def.id), "{} should read active", def.id);
        }
    }

    #[test]
    fn applying_the_same_tweak_twice_changes_nothing() {
        for def in tweak_catalogue() {
            let edits = edits_on(&def);
            if edits.is_empty() {
                continue;
            }
            for mut files in [seeded_pak(&edits, "999"), empty_pak()] {
                apply_edits_to_layers(&mut files, &edits);
                let once: Vec<String> = files.iter().map(|f| f.content.clone()).collect();
                apply_edits_to_layers(&mut files, &edits);
                let twice: Vec<String> = files.iter().map(|f| f.content.clone()).collect();
                assert_eq!(once, twice, "{} is not idempotent", def.id);
            }
        }
    }

    #[test]
    fn every_tweak_survives_an_on_off_on_cycle() {
        for def in tweak_catalogue() {
            let on = edits_on(&def);
            let Some(off) = edits_off(&def) else {
                continue;
            };
            if on.is_empty() {
                continue;
            }
            let mut files = seeded_pak(&on, "999");
            apply_edits_to_layers(&mut files, &on);
            apply_edits_to_layers(&mut files, &off);
            assert!(!is_active(&files, &def.id), "{} stuck on", def.id);
            apply_edits_to_layers(&mut files, &on);
            assert!(is_active(&files, &def.id), "{} stuck off", def.id);
            for edit in &on {
                assert_edit_landed(&files, edit, &format!("{} ON again", def.id));
            }
        }
    }

    // ── Layer routing ─────────────────────────────────────────────────

    /// Every file the editor manages gets the value, so a pak with several config files shows
    /// the tweak wherever you open it and no stale copy is left to shadow the others.
    #[test]
    fn a_set_reaches_every_file() {
        let mut files = empty_pak();
        apply_edits_to_layers(&mut files, &[edit("r.Foo", Some("1"))]);
        assert_eq!(
            key_hits(&files, "r.Foo"),
            vec![
                (PakIniTarget::BaseEngine, "1".into()),
                (PakIniTarget::Engine, "1".into()),
                (PakIniTarget::WindowsEngine, "1".into()),
                (PakIniTarget::BaseDeviceProfiles, "1".into()),
                (PakIniTarget::DeviceProfiles, "1".into()),
            ]
        );
        assert!(
            files[4].content.contains("+CVars=r.Foo=1"),
            "device profile CVars need the +CVars= prefix:\n{}",
            files[4].content
        );
        assert!(
            files[0].content.contains("[ConsoleVariables]\r\nr.Foo=1"),
            "engine CVars land in [ConsoleVariables]:\n{}",
            files[0].content
        );
    }

    #[test]
    fn a_stale_value_is_overwritten_rather_than_duplicated() {
        let mut files = empty_pak();
        files[0].content = "[ConsoleVariables]\r\nr.Foo=0\r\n".into();

        apply_edits_to_layers(&mut files, &[edit("r.Foo", Some("1"))]);

        assert_eq!(hits_in(&files, PakIniTarget::BaseEngine, "r.Foo"), 1);
        assert!(!files[0].content.contains("r.Foo=0"));
    }

    #[test]
    fn a_removal_clears_all_five_files() {
        let mut files = seeded_pak(&[edit("r.Foo", None)], "3");
        apply_edits_to_layers(&mut files, &[edit("r.Foo", None)]);
        assert!(key_hits(&files, "r.Foo").is_empty());
    }

    #[test]
    fn a_set_only_reaches_the_files_the_pak_actually_ships() {
        let mut files: Vec<LayerFile> = empty_pak().into_iter().take(3).collect();
        apply_edits_to_layers(&mut files, &[edit("r.Foo", Some("1"))]);
        assert_eq!(
            key_hits(&files, "r.Foo"),
            vec![
                (PakIniTarget::BaseEngine, "1".into()),
                (PakIniTarget::Engine, "1".into()),
                (PakIniTarget::WindowsEngine, "1".into()),
            ]
        );
    }

    #[test]
    fn base_device_profiles_is_written_like_any_other_layer() {
        let mut files: Vec<LayerFile> = empty_pak().into_iter().take(4).collect();
        apply_edits_to_layers(&mut files, &[edit("r.Foo", Some("1"))]);
        assert_eq!(
            hits_in(&files, PakIniTarget::BaseDeviceProfiles, "r.Foo"),
            1
        );
        assert!(files[3].content.contains("+CVars=r.Foo=1"));
    }

    #[test]
    fn engine_section_settings_never_reach_device_profiles() {
        let mut files = empty_pak();
        let edit = PakTweakEdit {
            key: "ApplicationScale".into(),
            value: Some("1.5".into()),
            engine_section: Some("/Script/Engine.UserInterfaceSettings".into()),
        };
        apply_edits_to_layers(&mut files, &[edit]);

        assert_eq!(
            key_hits(&files, "ApplicationScale"),
            vec![
                (PakIniTarget::BaseEngine, "1.5".into()),
                (PakIniTarget::Engine, "1.5".into()),
                (PakIniTarget::WindowsEngine, "1.5".into()),
            ],
            "an engine-section setting reaches every engine file, never a device profile"
        );
        assert!(
            files[2]
                .content
                .contains("[/Script/Engine.UserInterfaceSettings]")
        );
    }

    #[test]
    fn engine_section_removals_clear_every_engine_file() {
        let seed = PakTweakEdit {
            key: "MaxClientRate".into(),
            value: Some("300000".into()),
            engine_section: Some("/Script/OnlineSubsystemUtils.IpNetDriver".into()),
        };
        let mut files = seeded_pak(std::slice::from_ref(&seed), "300000");
        let removal = PakTweakEdit {
            value: None,
            ..seed
        };
        apply_edits_to_layers(&mut files, &[removal]);
        assert!(key_hits(&files, "MaxClientRate").is_empty());
    }

    // ── Duplicate files inside one layer ──────────────────────────────

    #[test]
    fn duplicate_files_in_one_layer_are_all_cleaned() {
        // A pak shipping Engine/Config/Windows/BaseWindowsEngine.ini next to
        // Marvel/Config/Windows/WindowsEngine.ini: both are WindowsEngine, both are live.
        let mut files = vec![
            layer(
                PakIniTarget::WindowsEngine,
                "[ConsoleVariables]\r\nr.MipMapLODBias=15\r\n".into(),
            ),
            layer(
                PakIniTarget::WindowsEngine,
                "[ConsoleVariables]\r\nr.MipMapLODBias=15\r\n".into(),
            ),
        ];
        apply_edits_to_layers(&mut files, &[edit("r.MipMapLODBias", None)]);
        assert!(key_hits(&files, "r.MipMapLODBias").is_empty());
    }

    #[test]
    fn a_new_key_reaches_both_files_of_a_shared_layer() {
        let mut files = vec![
            layer(PakIniTarget::WindowsEngine, "[ConsoleVariables]\r\n".into()),
            layer(PakIniTarget::WindowsEngine, "[ConsoleVariables]\r\n".into()),
        ];
        apply_edits_to_layers(&mut files, &[edit("r.Foo", Some("1"))]);
        assert!(files[0].content.contains("r.Foo=1"));
        assert!(files[1].content.contains("r.Foo=1"));
    }

    // ── What a removal reaches ────────────────────────────────────────

    /// Every section, not just the Windows profiles. A config mod writes whatever sections it
    /// likes: one seen in the wild carries `r.MipMapLODBias=15` under `[ConsoleVariables]` of its
    /// device profiles file and nowhere else, so a scan limited to device profile sections
    /// reported the tweak as already applied and then changed nothing.
    #[test]
    fn a_removal_reaches_every_section_of_a_device_profiles_file() {
        let mut files = vec![layer(
            PakIniTarget::DeviceProfiles,
            concat!(
                "[Windows DeviceProfile]\r\n",
                "+CVars=r.MipMapLODBias=15\r\n\r\n",
                "[IOS DeviceProfile]\r\n",
                "+CVars=r.MipMapLODBias=15\r\n\r\n",
                "[ConsoleVariables]\r\n",
                "r.MipMapLODBias=15\r\n"
            )
            .into(),
        )];
        apply_edits_to_layers(&mut files, &[edit("r.MipMapLODBias", None)]);

        assert_eq!(
            files[0].content.matches("r.MipMapLODBias").count(),
            0,
            "a survivor anywhere makes the tweak a no-op:\n{}",
            files[0].content
        );
        // The sections themselves stay; only the assignments go.
        assert!(files[0].content.contains("[IOS DeviceProfile]"));
        assert!(files[0].content.contains("[ConsoleVariables]"));
    }

    #[test]
    fn commented_out_lines_are_left_alone() {
        let mut files = vec![layer(
            PakIniTarget::DeviceProfiles,
            "[Windows DeviceProfile]\r\n;+CVars=r.MipMapLODBias=15\r\n+CVars=r.MipMapLODBias=15\r\n"
                .into(),
        )];
        apply_edits_to_layers(&mut files, &[edit("r.MipMapLODBias", None)]);
        assert_eq!(
            files[0].content.matches("r.MipMapLODBias").count(),
            1,
            "the commented copy is documentation, not a setting:\n{}",
            files[0].content
        );
    }

    // ── Where a set lands inside a device profiles file ───────────────

    #[test]
    fn a_set_collapses_inherited_profiles_into_the_one_the_game_runs() {
        let mut files = vec![layer(
            PakIniTarget::DeviceProfiles,
            concat!(
                "[Windows DeviceProfile]\r\n",
                "+CVars=r.Foo=0\r\n\r\n",
                "[WindowsClient DeviceProfile]\r\n",
                "+CVars=r.Foo=0\r\n"
            )
            .into(),
        )];
        apply_edits_to_layers(&mut files, &[edit("r.Foo", Some("1"))]);

        assert_eq!(
            key_hits(&files, "r.Foo"),
            vec![(PakIniTarget::DeviceProfiles, "1".into())]
        );
        let content = &files[0].content;
        let windows = content
            .find("[Windows DeviceProfile]")
            .expect("windows section");
        let client = content
            .find("[WindowsClient DeviceProfile]")
            .expect("client section");
        let value = content.find("+CVars=r.Foo=1").expect("the surviving line");
        assert!(
            value > windows && value < client,
            "the value belongs in the profile the others inherit from:\n{content}"
        );
    }

    #[test]
    fn a_set_reaches_the_primary_profile_even_when_the_key_lives_elsewhere() {
        let mut files = vec![layer(
            PakIniTarget::DeviceProfiles,
            concat!(
                "[Windows DeviceProfile]\r\n",
                "DeviceType=Windows\r\n\r\n",
                "[WindowsClient DeviceProfile]\r\n",
                "+CVars=r.Foo=0\r\n"
            )
            .into(),
        )];
        apply_edits_to_layers(&mut files, &[edit("r.Foo", Some("1"))]);

        let content = &files[0].content;
        assert_eq!(key_hits(&files, "r.Foo").len(), 1);
        let value = content.find("+CVars=r.Foo=1").expect("the moved line");
        let client = content
            .find("[WindowsClient DeviceProfile]")
            .expect("client section");
        assert!(
            value < client,
            "the override should move up to the profile the client runs:\n{content}"
        );
    }

    // ── Merged read priority ──────────────────────────────────────────

    #[test]
    fn higher_layers_win_the_merged_read() {
        let cases = [(0usize, 1usize), (1, 2), (2, 3), (3, 4)];
        for (lower, higher) in cases {
            let mut files = empty_pak();
            let low_content = if files[lower].target.is_engine() {
                "[ConsoleVariables]\r\nr.Shared=1\r\n".to_string()
            } else {
                "[Windows DeviceProfile]\r\n+CVars=r.Shared=1\r\n".to_string()
            };
            let high_content = if files[higher].target.is_engine() {
                "[ConsoleVariables]\r\nr.Shared=9\r\n".to_string()
            } else {
                "[Windows DeviceProfile]\r\n+CVars=r.Shared=9\r\n".to_string()
            };
            files[lower].content = low_content;
            files[higher].content = high_content;

            let merged = merged(&files);
            assert!(
                merged.contains("r.Shared=9"),
                "{:?} should win over {:?}:\n{merged}",
                files[higher].target,
                files[lower].target
            );
            assert!(!merged.contains("r.Shared=1"), "stale value in:\n{merged}");
        }
    }
    // ── Presets: many tweaks applied in one pass ──────────────────────
    //
    // A preset is a saved list of tweak states. The app turns it into edits with
    // `edits_for_settings` and hands the whole batch to one apply, so these check that a batch
    // behaves like the sum of its parts: no tweak clobbers another, order does not matter, and
    // what comes back out of detection is what was asked for.

    fn is_remove_only(def: &TweakDefinition) -> bool {
        matches!(
            def.kind,
            TweakKind::RemoveLines {
                remove_only: true,
                ..
            }
        )
    }

    /// A preset covering the whole catalogue, with `enabled` deciding each tweak.
    ///
    /// Remove-only tweaks are left out when off, matching the app: their lines are gone for good,
    /// so asking for the off state is rejected rather than silently ignored.
    fn preset(enabled: impl Fn(usize, &TweakDefinition) -> bool) -> Vec<TweakSetting> {
        tweak_catalogue()
            .iter()
            .enumerate()
            .filter_map(|(index, def)| {
                let on = enabled(index, def);
                if !on && is_remove_only(def) {
                    return None;
                }
                Some(TweakSetting {
                    id: def.id.clone(),
                    enabled: on,
                    value: if on { slider_target(def) } else { None },
                })
            })
            .collect()
    }

    /// The preset the app would save off a pak in its current state.
    fn preset_from(files: &[LayerFile]) -> Vec<TweakSetting> {
        let detected = states(files);
        tweak_catalogue()
            .iter()
            .filter_map(|def| {
                let state = detected.iter().find(|s| s.id == def.id);
                let enabled = state.is_some_and(|s| s.active);
                if !enabled && is_remove_only(def) {
                    return None;
                }
                Some(TweakSetting {
                    id: def.id.clone(),
                    enabled,
                    value: state.and_then(|s| s.current_value.clone()),
                })
            })
            .collect()
    }

    fn apply_preset(files: &mut [LayerFile], settings: &[TweakSetting]) {
        let edits = edits_for_settings(settings)
            .unwrap_or_else(|e| panic!("preset failed to translate: {e}"));
        apply_edits_to_layers(files, &edits);
    }

    fn states(files: &[LayerFile]) -> Vec<TweakState> {
        detect_tweaks_unscoped(&merged(files))
    }

    fn assert_matches_request(files: &[LayerFile], settings: &[TweakSetting], what: &str) {
        let detected = states(files);
        for setting in settings {
            let state = detected
                .iter()
                .find(|s| s.id == setting.id)
                .unwrap_or_else(|| panic!("no detected state for {}", setting.id));
            assert_eq!(
                state.active, setting.enabled,
                "{what}: {} was requested {} but reads {}",
                setting.id, setting.enabled, state.active
            );
        }
    }

    fn contents(files: &[LayerFile]) -> Vec<String> {
        files.iter().map(|f| f.content.clone()).collect()
    }

    /// Every key any tweak in the catalogue can write.
    fn all_tweak_keys() -> Vec<String> {
        let mut keys = Vec::new();
        for def in tweak_catalogue() {
            for edit in edits_on(&def) {
                if !keys.contains(&edit.key) {
                    keys.push(edit.key);
                }
            }
        }
        keys
    }

    /// A pak carrying every key a tweak can write, set to a value no tweak uses, in every file
    /// and every live section. Whatever the preset asks for has to win over all of it.
    fn dirty_pak() -> Vec<LayerFile> {
        let seeds: Vec<PakTweakEdit> = tweak_catalogue().iter().flat_map(edits_on).collect();
        seeded_pak(&seeds, "999")
    }

    /// Which tweaks a preset turns on, by catalogue position.
    type Pattern = fn(usize, &TweakDefinition) -> bool;

    #[test]
    fn a_whole_catalogue_preset_lands_exactly_as_requested() {
        let cases: [(&str, Pattern); 4] = [
            ("all on", |_, _| true),
            ("all off", |_, _| false),
            ("alternating", |i, _| i % 2 == 0),
            ("inverse alternating", |i, _| i % 2 == 1),
        ];
        for (label, pattern) in cases {
            let settings = preset(pattern);
            for (start, mut files) in [("clean", empty_pak()), ("dirty", dirty_pak())] {
                apply_preset(&mut files, &settings);
                assert_matches_request(&files, &settings, &format!("{label} on a {start} pak"));
            }
        }
    }

    #[test]
    fn preset_entry_order_does_not_change_the_result() {
        let forward = preset(|i, _| i % 3 != 0);
        let mut reversed = forward.clone();
        reversed.reverse();

        let mut a = dirty_pak();
        let mut b = dirty_pak();
        apply_preset(&mut a, &forward);
        apply_preset(&mut b, &reversed);

        assert_eq!(
            contents(&a),
            contents(&b),
            "a preset is a set of independent tweaks, so the list order must not matter"
        );
    }

    #[test]
    fn applying_a_preset_twice_changes_nothing() {
        let settings = preset(|i, _| i % 2 == 0);
        for mut files in [empty_pak(), dirty_pak()] {
            apply_preset(&mut files, &settings);
            let once = contents(&files);
            apply_preset(&mut files, &settings);
            assert_eq!(once, contents(&files), "a preset must be idempotent");
        }
    }

    #[test]
    fn a_preset_only_touches_the_tweaks_it_names() {
        let everything_on = preset(|_, _| true);
        let mut files = empty_pak();
        apply_preset(&mut files, &everything_on);

        let partial: Vec<TweakSetting> = everything_on
            .iter()
            .filter(|s| matches!(s.id.as_str(), "cas_sharpening" | "font_aa"))
            .map(|s| TweakSetting {
                enabled: false,
                value: None,
                ..s.clone()
            })
            .collect();
        assert_eq!(partial.len(), 2, "both tweaks should be in the catalogue");
        apply_preset(&mut files, &partial);

        assert_matches_request(&files, &partial, "partial preset");
        let untouched: Vec<TweakSetting> = everything_on
            .iter()
            .filter(|s| !partial.iter().any(|p| p.id == s.id))
            .cloned()
            .collect();
        assert_matches_request(&files, &untouched, "tweaks the partial preset never named");
    }

    #[test]
    fn a_preset_lands_the_same_state_on_a_dirty_pak_as_on_a_clean_one() {
        let settings = preset(|i, _| i % 2 == 1);

        let mut clean = empty_pak();
        let mut dirty = dirty_pak();
        apply_preset(&mut clean, &settings);
        apply_preset(&mut dirty, &settings);

        let clean_states = states(&clean);
        let dirty_states = states(&dirty);
        for def in tweak_catalogue() {
            let a = clean_states
                .iter()
                .find(|s| s.id == def.id)
                .map(|s| s.active);
            let b = dirty_states
                .iter()
                .find(|s| s.id == def.id)
                .map(|s| s.active);
            assert_eq!(
                a, b,
                "{}: a preset should fully define the state, whatever the pak started with",
                def.id
            );
        }
    }

    /// What the app does when you save a preset off one pak and apply it to another: the state it
    /// captured has to reproduce itself, values included.
    #[test]
    fn a_preset_saved_off_a_pak_reproduces_that_pak_state() {
        let mut source = dirty_pak();
        apply_preset(&mut source, &preset(|i, _| i % 3 != 1));

        let saved = preset_from(&source);
        let mut target = empty_pak();
        apply_preset(&mut target, &saved);

        assert_matches_request(&target, &saved, "preset saved off another pak");
        let detected = states(&target);
        let mut compared = 0;
        for setting in saved.iter().filter(|s| s.enabled && s.value.is_some()) {
            let state = detected
                .iter()
                .find(|s| s.id == setting.id)
                .unwrap_or_else(|| panic!("no state for {}", setting.id));
            assert_eq!(
                state.current_value, setting.value,
                "{}: the value the preset captured should come back",
                setting.id
            );
            compared += 1;
        }
        assert!(compared > 5, "only {compared} values were checked");
    }

    /// A tweak that is on by default reads active with no value behind it. The preset stores that
    /// as `value: null`, and applying it has to write the value out rather than leave the file bare.
    #[test]
    fn a_preset_makes_a_default_on_tweak_explicit() {
        let source = empty_pak();
        let saved = preset_from(&source);
        let cas = saved
            .iter()
            .find(|s| s.id == "cas_sharpening")
            .expect("cas_sharpening is in the preset");
        assert!(
            cas.enabled && cas.value.is_none(),
            "on by default, no value"
        );

        let mut target = dirty_pak();
        apply_preset(&mut target, &saved);

        let hits = key_hits(&target, "r.PostProcessing.EnableCAS");
        assert!(
            hits.iter().all(|(_, v)| v == "1"),
            "the default has to be written out, not left to the stale value:\n{hits:#?}"
        );
        for target_layer in PakIniTarget::ALL {
            assert!(
                hits.iter().any(|(t, _)| *t == target_layer),
                "{target_layer:?} kept its stale value:\n{hits:#?}"
            );
        }
        assert_matches_request(&target, &saved, "preset from a bare pak");
    }

    /// The app disables engine-only tweaks for a pak with no engine file, but a preset can still
    /// name them. Nothing is written for those, so the applied state does not match the request.
    #[test]
    fn engine_only_tweaks_in_a_preset_do_nothing_without_an_engine_file() {
        let mut files: Vec<LayerFile> = empty_pak().into_iter().skip(3).collect();
        assert!(files.iter().all(|f| !f.target.is_engine()));

        let settings = vec![TweakSetting {
            id: "application_scale".into(),
            enabled: true,
            value: Some("1.5".into()),
        }];
        apply_preset(&mut files, &settings);

        assert!(
            key_hits(&files, "ApplicationScale").is_empty(),
            "an engine-section setting has nowhere to go in a device profiles file"
        );
        assert!(
            !states(&files)
                .iter()
                .any(|s| s.id == "application_scale" && s.active),
            "and it reads back off, so the request silently did not take"
        );
    }

    /// Re-saving and re-applying must settle, not drift: the second pass has nothing left to do.
    #[test]
    fn saving_and_reapplying_a_preset_reaches_a_fixpoint() {
        let mut files = dirty_pak();
        apply_preset(&mut files, &preset(|i, _| i % 2 == 0));
        let before = contents(&files);

        let saved = preset_from(&files);
        apply_preset(&mut files, &saved);

        assert_eq!(
            before,
            contents(&files),
            "re-applying what the pak already reads as should write nothing"
        );
    }

    #[test]
    fn every_file_agrees_on_every_key_after_a_preset() {
        let mut files = dirty_pak();
        apply_preset(&mut files, &preset(|i, _| i % 2 == 0));

        let mut checked = 0;
        for key in all_tweak_keys() {
            let hits = key_hits(&files, &key);
            let values: Vec<&String> = hits.iter().map(|(_, v)| v).collect();
            if let Some(first) = values.first() {
                assert!(
                    values.iter().all(|v| v == first),
                    "{key} disagrees across files, so which one applies depends on load order:\n{hits:#?}"
                );
                checked += 1;
            }
        }
        assert!(checked > 10, "only {checked} keys survived the preset");
    }

    #[test]
    fn a_preset_naming_the_same_tweak_twice_is_rejected() {
        let mut settings = preset(|_, _| true);
        let duplicate = settings[0].clone();
        settings.push(duplicate);
        assert!(edits_for_settings(&settings).is_err());
    }

    #[test]
    fn a_preset_with_an_unknown_tweak_writes_nothing() {
        let mut settings = preset(|_, _| true);
        settings.push(TweakSetting {
            id: "tweak_from_a_newer_build".into(),
            enabled: true,
            value: None,
        });

        let files = empty_pak();
        let before = contents(&files);
        assert!(
            edits_for_settings(&settings).is_err(),
            "an id this build does not know must not be silently skipped"
        );
        assert_eq!(
            before,
            contents(&files),
            "translation fails before anything is written"
        );
    }

    /// A preset saved off a pak whose slider sits outside the catalogue range cannot be applied
    /// anywhere: `edits_for_settings` rejects the value, and the whole batch fails with it.
    #[test]
    fn a_preset_carrying_an_out_of_range_slider_is_rejected_whole() {
        let mut source = empty_pak();
        source[4].content = "[Windows DeviceProfile]\r\n+CVars=r.TeamOutline.LineMode=5\r\n".into();

        let saved = preset_from(&source);
        let captured = saved
            .iter()
            .find(|s| s.id == "team_outline_line_mode")
            .expect("slider is in the preset");
        assert_eq!(captured.value.as_deref(), Some("5"));

        let err = edits_for_settings(&saved).expect_err("out-of-range value is rejected");
        assert!(err.contains("outside"), "{err}");
    }
}
