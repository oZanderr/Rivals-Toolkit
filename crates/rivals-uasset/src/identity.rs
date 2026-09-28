//! Saving a package under another name: the name it is known by, the paths inside it that name
//! the package itself, and, when asked, the objects named after it.
//!
//! A package's IoStore id comes from its stored name, not from where it sits in a container, so
//! the name is what makes a copy a new asset. Paths the package holds to itself are FNames, found
//! where the reader recorded them and pointed at the new name one use at a time, so an FName's
//! number and any other use of the same name stay as they were. Paths written as strings are value
//! edits, made by the caller once this has run.

use retoc::legacy_asset::{FLegacyPackageHeader, FMinimalName, FObjectExport};
use serde::{Deserialize, Serialize};

use crate::edit::{EditOp, PatchedBundle, ScriptConstEdit, ValueEdit, kind_of};
use crate::package::ExportStatus;
use crate::package::{AssetBundle, ParsedPackage, header_size, read_header};
use crate::value::{PropertyEntry, PropertyValue};
use crate::write::{HeaderDraft, Splice, check_inline_bulk, rewrite};

/// Where a save puts the package instead of where it was read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveAs {
    /// The package name it is saved as, such as `/Game/Mods/MyThing/DA_Sword`.
    pub package: String,
    /// Rename the objects named after the package along with it: the asset itself, and for a
    /// Blueprint its class and class default object.
    #[serde(default = "renames_objects")]
    pub rename_objects: bool,
}

fn renames_objects() -> bool {
    true
}

/// How every path naming the package or one of its renamed objects changes.
#[derive(Debug, Clone)]
pub struct PathRename {
    pub from: String,
    pub to: String,
    /// Old path to new, longest first so an object's path is matched before its package's.
    pairs: Vec<(String, String)>,
    /// Top-level exports given new names, by index.
    objects: Vec<(usize, String)>,
    /// Old object name to new, for the asset half of a soft path.
    object_names: Vec<(String, String)>,
}

/// Characters a package name cannot hold.
const INVALID: &[char] = &[
    '\\', ':', '*', '?', '"', '<', '>', '|', '\'', ' ', ',', '.', '&', '!', '~', '@', '#', '\n',
    '\r', '\t',
];

impl PathRename {
    /// What saving `parsed` as `save_as` renames, or `None` when it already has that name.
    pub fn plan(parsed: &ParsedPackage, save_as: &SaveAs) -> Result<Option<Self>, String> {
        let to = save_as.package.trim().to_string();
        check_package_name(&to)?;
        let from = parsed.info.package_name.clone();
        if from.eq_ignore_ascii_case(&to) {
            return Ok(None);
        }
        if parsed
            .exports
            .iter()
            .any(|export| export.class_name == "World")
        {
            return Err(format!(
                "{from} is a level, whose package name is written into its world and every actor \
                 it places, so it cannot be saved under another path"
            ));
        }
        if let Some(import) = parsed.imports.iter().find(|import| {
            let package = import.path.split('.').next().unwrap_or(&import.path);
            package.eq_ignore_ascii_case(&to)
        }) {
            return Err(format!(
                "{from} imports {}, so saving it as {to} would make it import itself",
                import.path
            ));
        }
        let mut pairs = vec![(from.clone(), to.clone())];
        let mut objects = Vec::new();
        let mut object_names = Vec::new();
        let short = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
        let (old, new) = (short(&from), short(&to));
        if save_as.rename_objects && !old.eq_ignore_ascii_case(&new) {
            let renames = [
                (old.clone(), new.clone()),
                (format!("{old}_C"), format!("{new}_C")),
                (format!("Default__{old}_C"), format!("Default__{new}_C")),
            ];
            for (index, export) in parsed.exports.iter().enumerate() {
                if export.outer_index != 0 {
                    continue;
                }
                let Some((_, renamed)) = renames
                    .iter()
                    .find(|(was, _)| was.eq_ignore_ascii_case(&export.object_name))
                else {
                    continue;
                };
                if let Some(taken) = parsed.exports.iter().find(|other| {
                    other.outer_index == 0 && other.object_name.eq_ignore_ascii_case(renamed)
                }) {
                    return Err(format!(
                        "{from} already holds {}, so {} cannot take its name",
                        taken.object_name, export.object_name
                    ));
                }
                pairs.push((
                    format!("{from}.{}", export.object_name),
                    format!("{to}.{renamed}"),
                ));
                objects.push((index, renamed.clone()));
                object_names.push((export.object_name.clone(), renamed.clone()));
            }
        }
        pairs.sort_by_key(|(was, _)| std::cmp::Reverse(was.len()));
        Ok(Some(Self {
            from,
            to,
            pairs,
            objects,
            object_names,
        }))
    }

    /// The paths an edit of `package`'s export table moves, old to new, as a rename of paths inside
    /// a package that keeps its own name. What other packages hold of them follows the same way.
    pub fn of_paths(package: &str, repathed: &[(String, String)]) -> Self {
        let last = |path: &str| path.rsplit(['.', ':']).next().unwrap_or(path).to_string();
        let mut pairs: Vec<(String, String)> = repathed.to_vec();
        pairs.sort_by_key(|(was, _)| std::cmp::Reverse(was.len()));
        let object_names = repathed
            .iter()
            .map(|(was, now)| (last(was), last(now)))
            .filter(|(was, now)| was != now)
            .collect();
        Self {
            from: package.to_string(),
            to: package.to_string(),
            pairs,
            objects: Vec::new(),
            object_names,
        }
    }

    /// `text` renamed, when it is the package's path or a path inside it.
    pub fn apply(&self, text: &str) -> Option<String> {
        self.pairs.iter().find_map(|(was, now)| {
            let head = text.get(..was.len())?;
            let rest = &text[was.len()..];
            (head.eq_ignore_ascii_case(was) && (rest.is_empty() || rest.starts_with(['.', ':'])))
                .then(|| format!("{now}{rest}"))
        })
    }

    /// Every path inside `text` renamed, wherever it stands on its own.
    pub fn apply_within(&self, text: &str) -> String {
        rename_within(&self.pairs, text)
    }

    /// Old path to new, for a comparison that lets the rename through.
    pub(crate) fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }

    /// The new name of a renamed top-level object, by its old one.
    pub(crate) fn object_name(&self, name: &str) -> Option<&str> {
        self.object_names
            .iter()
            .find(|(was, _)| was.eq_ignore_ascii_case(name))
            .map(|(_, now)| now.as_str())
    }
}

/// `text` with every path in `pairs` that stands on its own replaced, longest first: one that runs
/// on into more of a path is another path.
pub(crate) fn rename_within(pairs: &[(String, String)], text: &str) -> String {
    let part_of_path = |c: char| c.is_alphanumeric() || matches!(c, '_' | '/' | '-');
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    'scan: while at < text.len() {
        let before = text[..at].chars().next_back();
        if !before.is_some_and(part_of_path) {
            for (was, now) in pairs {
                let Some(head) = text.get(at..at + was.len()) else {
                    continue;
                };
                let after = text[at + was.len()..].chars().next();
                if head.eq_ignore_ascii_case(was) && !after.is_some_and(part_of_path) {
                    out.push_str(now);
                    at += was.len();
                    continue 'scan;
                }
            }
        }
        let Some(c) = text[at..].chars().next() else {
            break;
        };
        out.push(c);
        at += c.len_utf8();
    }
    out
}

/// A long package name UE accepts: a mount and a path under it, with nothing a path cannot hold.
fn check_package_name(name: &str) -> Result<(), String> {
    let well_formed = name.starts_with('/')
        && !name.ends_with('/')
        && name.split('/').skip(1).count() >= 2
        && name.split('/').skip(1).all(|part| !part.is_empty())
        && !name.contains(INVALID);
    if well_formed {
        Ok(())
    } else {
        Err(format!(
            "{name} is not a package name: it takes the form /Mount/Path/Name, such as \
             /Game/Mods/MyThing/DA_Sword, with no dots, spaces or colons"
        ))
    }
}

/// A splice for each name the reader recorded in export data that is one of the renamed paths, or
/// the asset half of a soft path whose package half is the renamed package, pointing it at the new
/// name at its own width.
fn name_splices(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    rename: &PathRename,
    package: &FLegacyPackageHeader,
    total: u64,
    names: &mut retoc::legacy_asset::FPackageNameMap,
) -> Result<Vec<Splice>, String> {
    let mut offsets: Vec<u64> = parsed
        .exports
        .iter()
        .flat_map(|export| export.name_refs.iter().copied())
        .collect();
    offsets.sort_unstable();
    offsets.dedup();
    let read = |at: u64| -> Result<FMinimalName, String> {
        let start = at
            .checked_sub(total)
            .and_then(|local| usize::try_from(local).ok())
            .ok_or_else(|| format!("a name is recorded at {at:#X}, inside the package header"))?;
        let bytes = bundle
            .exports
            .get(start..start + 8)
            .ok_or_else(|| format!("a name is recorded at {at:#X}, past the export data"))?;
        Ok(FMinimalName {
            index: i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            number: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    };
    let text_at = |at: u64| -> Result<String, String> {
        package
            .name_map
            .get(read(at)?)
            .map(|name| name.into_owned())
            .map_err(|e| format!("the name at {at:#X}: {e}"))
    };
    let mut splices = Vec::new();
    for (position, &at) in offsets.iter().enumerate() {
        let text = text_at(at)?;
        let renamed = match rename.apply(&text) {
            Some(renamed) => Some(renamed),
            None => {
                let package_half = position
                    .checked_sub(1)
                    .map(|previous| offsets[previous])
                    .filter(|&previous| previous + 8 == at)
                    .map(text_at)
                    .transpose()?;
                package_half
                    .filter(|half| half.eq_ignore_ascii_case(&rename.from))
                    .and_then(|_| rename.object_name(&text))
                    .map(str::to_string)
            }
        };
        if let Some(renamed) = renamed {
            let stored = names.store(&renamed);
            let mut bytes = stored.index.to_le_bytes().to_vec();
            bytes.extend_from_slice(&stored.number.to_le_bytes());
            splices.push(Splice {
                start: at,
                end: at + 8,
                bytes,
            });
        }
    }
    Ok(splices)
}

/// Gives the package its new name: the stored name, every recorded FName that is a path into the
/// package, the asset half of a soft path whose package half is this package, and the names of the
/// renamed objects. Nothing moves; each name is rewritten at its own width.
pub(crate) fn rename_package(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    rename: &PathRename,
) -> Result<PatchedBundle, String> {
    let package: FLegacyPackageHeader = read_header(bundle)?;
    let total = header_size(bundle)?;
    let mut names = package.name_map.clone();
    let splices = name_splices(bundle, parsed, rename, &package, total, &mut names)?;
    let exports: Vec<FObjectExport> = package
        .exports
        .iter()
        .enumerate()
        .map(
            |(index, export)| match rename.objects.iter().find(|(at, _)| *at == index) {
                Some((_, name)) => FObjectExport {
                    object_name: names.store(name),
                    ..export.clone()
                },
                None => export.clone(),
            },
        )
        .collect();
    let rewritten = rewrite(
        bundle,
        &splices,
        HeaderDraft {
            names: Some(names),
            exports: Some(exports),
            package_name: Some(rename.to.clone()),
            ..Default::default()
        },
    )?;
    check_inline_bulk(&AssetBundle {
        asset: &rewritten.asset,
        exports: &rewritten.exports,
    })?;
    Ok(PatchedBundle {
        asset: rewritten.asset,
        exports: rewritten.exports,
        applied: Vec::new(),
        bulk: None,
        optional_bulk: None,
        notes: Vec::new(),
    })
}

/// Points another package at the renamed paths: its imports of them take their new names in place,
/// which keeps each one the object it was, a class included, and every name it holds of them follows
/// as [`rename_package`] makes them follow. Its own name stays. An import whose object moved under
/// another outer cannot be renamed in place, and comes back as a retarget to make after.
pub fn rename_references(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    rename: &PathRename,
) -> Result<(PatchedBundle, Vec<crate::header_edit::ImportEdit>), String> {
    let package: FLegacyPackageHeader = read_header(bundle)?;
    let total = header_size(bundle)?;
    let mut names = package.name_map.clone();
    let splices = name_splices(bundle, parsed, rename, &package, total, &mut names)?;
    let parent =
        |path: &str| -> Option<String> { path.rfind(['.', ':']).map(|at| path[..at].to_string()) };
    let last = |path: &str| path.rsplit(['.', ':']).next().unwrap_or(path).to_string();
    let mut imports = package.imports.clone();
    let mut moves = Vec::new();
    for (at, import) in parsed.imports.iter().enumerate() {
        let Some(renamed) = rename.apply(&import.path) else {
            continue;
        };
        let Some(entry) = imports.get_mut(at) else {
            continue;
        };
        match parent(&import.path) {
            None => entry.object_name = names.store(&renamed),
            Some(outer) => {
                let outer_now = rename.apply(&outer).unwrap_or(outer);
                if parent(&renamed).is_some_and(|now| now.eq_ignore_ascii_case(&outer_now)) {
                    entry.object_name = names.store(&last(&renamed));
                } else {
                    moves.push(crate::header_edit::ImportEdit::Retarget {
                        import: at as u32,
                        path: renamed,
                        class: None,
                    });
                }
            }
        }
    }
    let rewritten = rewrite(
        bundle,
        &splices,
        HeaderDraft {
            names: Some(names),
            imports: Some(imports),
            ..Default::default()
        },
    )?;
    check_inline_bulk(&AssetBundle {
        asset: &rewritten.asset,
        exports: &rewritten.exports,
    })?;
    Ok((
        PatchedBundle {
            asset: rewritten.asset,
            exports: rewritten.exports,
            applied: Vec::new(),
            bulk: None,
            optional_bulk: None,
            notes: Vec::new(),
        },
        moves,
    ))
}

/// Whether `parsed` may name any of the renamed paths: by an import, by a name that is one, or by
/// naming the package they are in, as the package half of a soft path does.
pub fn names_renamed_paths(parsed: &ParsedPackage, rename: &PathRename) -> bool {
    parsed
        .imports
        .iter()
        .any(|import| rename.apply(&import.path).is_some())
        || parsed
            .names
            .iter()
            .any(|name| rename.apply(name).is_some() || name.eq_ignore_ascii_case(&rename.from))
}

/// The value edits that point the paths the package writes as strings at its new name: plain
/// strings and the game's string-backed soft paths. Run on the package once [`rename_package`] has,
/// so the FName-backed paths already read renamed and only the strings are left.
pub fn identity_value_edits(parsed: &ParsedPackage, rename: &PathRename) -> Vec<ValueEdit> {
    let mut edits = Vec::new();
    for export in &parsed.exports {
        collect_edits(&export.properties, rename, &mut edits);
        collect_edits(&export.defaults, rename, &mut edits);
        if let Some(table) = &export.data_table {
            for row in &table.rows {
                collect_edits(&row.fields, rename, &mut edits);
            }
        }
    }
    edits
}

fn path_text(value: &PropertyValue) -> Option<&str> {
    match value {
        PropertyValue::Str { value } => Some(value),
        PropertyValue::SoftObject { path } => Some(path),
        _ => None,
    }
}

fn collect_edits(entries: &[PropertyEntry], rename: &PathRename, edits: &mut Vec<ValueEdit>) {
    for entry in entries {
        let Some((start, end)) = entry.span else {
            continue;
        };
        let edit = |op: EditOp| ValueEdit {
            offset: start,
            expect_name: entry.name.clone(),
            expect_element: entry.element,
            expect_kind: kind_of(&entry.value),
            op,
        };
        match &entry.value {
            value if end > start && path_text(value).is_some() => {
                if let Some(text) = path_text(value).and_then(|text| rename.apply(text)) {
                    edits.push(edit(EditOp::Set { text }));
                }
            }
            PropertyValue::Struct { fields, .. } => collect_edits(fields, rename, edits),
            PropertyValue::Map { entries } => {
                for (index, pair) in entries.iter().enumerate() {
                    match &pair.value {
                        PropertyValue::Struct { fields, .. } => {
                            collect_edits(fields, rename, edits)
                        }
                        value => {
                            if let Some(text) = path_text(value).and_then(|text| rename.apply(text))
                            {
                                edits.push(edit(EditOp::SetElement {
                                    index: index as u32,
                                    text,
                                }));
                            }
                        }
                    }
                }
            }
            PropertyValue::Array { items } | PropertyValue::Set { items } => {
                for (index, item) in items.iter().enumerate() {
                    match item {
                        PropertyValue::Struct { fields, .. } => {
                            collect_edits(fields, rename, edits)
                        }
                        item => {
                            if let Some(text) = path_text(item).and_then(|text| rename.apply(text))
                            {
                                edits.push(edit(EditOp::SetElement {
                                    index: index as u32,
                                    text,
                                }));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// The strings in bytecode that name the package, rewritten to name it by its new name where the
/// new one is as long as the old: a string in bytecode cannot change length without moving every
/// jump after it.
pub fn identity_script_edits(parsed: &ParsedPackage, rename: &PathRename) -> Vec<ScriptConstEdit> {
    let mut edits = Vec::new();
    for (export, statement, slot, renamed) in script_strings(parsed, rename) {
        if same_length(slot.kind, &slot.value, &renamed) {
            edits.push(ScriptConstEdit {
                export,
                statement,
                constant: slot.index,
                value: renamed,
            });
        }
    }
    edits
}

/// What saving under the new name leaves naming the old one: a bytecode string of another length,
/// a map key, and whatever a payload the reader does not follow holds.
pub fn identity_leftovers(parsed: &ParsedPackage, rename: &PathRename) -> Vec<String> {
    let mut notes = Vec::new();
    for (export, statement, slot, renamed) in script_strings(parsed, rename) {
        if !same_length(slot.kind, &slot.value, &renamed) {
            notes.push(format!(
                "{} still names {} in a string in its bytecode at {statement:#06X}: a bytecode \
                 string cannot change length",
                parsed.exports[export as usize].object_name, rename.from
            ));
        }
    }
    for export in &parsed.exports {
        let mut keys = Vec::new();
        map_keys(&export.properties, rename, &mut keys);
        map_keys(&export.defaults, rename, &mut keys);
        for key in keys {
            notes.push(format!(
                "{} keeps the map key {key}, which names {}: a key cannot be renamed in place",
                export.object_name, rename.from
            ));
        }
        if let ExportStatus::Payload { kind, .. } = &export.status
            && !export.names_complete
        {
            notes.push(format!(
                "{} holds {kind}, which the reader does not follow, so a name of {} inside it is \
                 left as it was",
                export.object_name, rename.from
            ));
        }
    }
    notes
}

/// Every string literal in the package's bytecode that names the package, with the export, the
/// statement and the literal it sits in, and what it reads renamed.
fn script_strings(
    parsed: &ParsedPackage,
    rename: &PathRename,
) -> Vec<(u32, u32, crate::kismet::LiteralSlot, String)> {
    let mut found = Vec::new();
    for export in &parsed.exports {
        let Some(script) = &export.script else {
            continue;
        };
        for statement in &script.statements {
            for slot in crate::kismet::literal_slots(&statement.expr) {
                if !matches!(slot.kind, "StringConst" | "UnicodeStringConst") {
                    continue;
                }
                let renamed = rename.apply_within(&slot.value);
                if renamed != slot.value {
                    found.push((export.index, statement.offset, slot, renamed));
                }
            }
        }
    }
    found
}

fn same_length(kind: &str, was: &str, now: &str) -> bool {
    if kind == "UnicodeStringConst" {
        was.encode_utf16().count() == now.encode_utf16().count()
    } else {
        was.chars().count() == now.chars().count()
    }
}

/// The keys of the maps in `entries` that name the package.
fn map_keys(entries: &[PropertyEntry], rename: &PathRename, keys: &mut Vec<String>) {
    for entry in entries {
        match &entry.value {
            PropertyValue::Map { entries: pairs } => {
                for pair in pairs {
                    if let Some(text) = path_text(&pair.key)
                        && rename.apply(text).is_some()
                    {
                        keys.push(text.to_string());
                    }
                    if let PropertyValue::Struct { fields, .. } = &pair.value {
                        map_keys(fields, rename, keys);
                    }
                }
            }
            PropertyValue::Struct { fields, .. } => map_keys(fields, rename, keys),
            PropertyValue::Array { items } | PropertyValue::Set { items } => {
                for item in items {
                    if let PropertyValue::Struct { fields, .. } = item {
                        map_keys(fields, rename, keys);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bytecode string is rewritten only at its own length, counted in the unit it is stored in.
    #[test]
    fn a_bytecode_string_keeps_its_length() {
        assert!(same_length("StringConst", "/Game/Aa.Aa", "/Game/Bb.Bb"));
        assert!(!same_length("StringConst", "/Game/A.A", "/Game/Mods/B.B"));
        assert!(same_length(
            "UnicodeStringConst",
            "/Game/\u{e9}.\u{e9}",
            "/Game/e.e"
        ));
        assert!(!same_length("StringConst", "/Game/\u{1F600}", "/Game/xy"));
    }

    #[test]
    fn a_path_is_renamed_where_it_stands_alone_and_not_inside_a_longer_one() {
        let pairs = vec![
            ("/Game/A.A".to_string(), "/Game/B.B".to_string()),
            ("/Game/A".to_string(), "/Game/B".to_string()),
        ];
        assert_eq!(
            rename_within(&pairs, "Object(/Game/A.A) and /Game/A.Other:Sub"),
            "Object(/Game/B.B) and /Game/B.Other:Sub"
        );
        assert_eq!(
            rename_within(&pairs, "/Game/AB.AB /Game/A/Deeper x/Game/A"),
            "/Game/AB.AB /Game/A/Deeper x/Game/A"
        );
    }
}
