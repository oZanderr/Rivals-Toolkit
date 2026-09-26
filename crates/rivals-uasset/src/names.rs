//! Dropping the names a package's name map holds that nothing in the package uses any more.
//!
//! A name stays behind when an edit replaces the only value naming it. Dropping it renumbers every
//! name after it, so each place a name is stored has to be known: the reader records every name it
//! reads in export data, and an export not read to its end may hold names nobody recorded.

use retoc::legacy_asset::{
    FLegacyPackageHeader, FMinimalName, FObjectExport, FObjectImport, FPackageNameMap,
};

use crate::package::{ExportStatus, ParsedPackage};
use crate::write::Splice;

/// A name map with only the names in use, and everything that points into it renumbered.
pub(crate) struct Compaction {
    pub names: FPackageNameMap,
    pub imports: Vec<FObjectImport>,
    pub exports: Vec<FObjectExport>,
    pub splices: Vec<Splice>,
}

/// The names nothing in the package uses, or why that cannot be known.
pub fn unused_names(
    parsed: &ParsedPackage,
    package: &FLegacyPackageHeader,
    exports: &[u8],
    total: u64,
) -> Result<Vec<String>, String> {
    let used = used_names(parsed, package, exports, total)?;
    Ok(package
        .name_map
        .raw_names()
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(name, _)| name.clone())
        .collect())
}

/// Plans the rewrite that keeps only the names in use, in the order they had.
pub(crate) fn compact_names(
    parsed: &ParsedPackage,
    package: &FLegacyPackageHeader,
    exports: &[u8],
    total: u64,
) -> Result<Compaction, String> {
    let used = used_names(parsed, package, exports, total)?;
    if used.iter().all(|used| *used) {
        return Err("every name in the package is in use, so there is nothing to drop".into());
    }
    let mut remap = vec![0i32; used.len()];
    let mut kept = Vec::new();
    for (index, (name, used)) in package.name_map.raw_names().iter().zip(&used).enumerate() {
        if *used {
            remap[index] = kept.len() as i32;
            kept.push(name.clone());
        }
    }
    let renumber = |name: FMinimalName| FMinimalName {
        index: remap[name.index as usize],
        number: name.number,
    };
    let imports = package
        .imports
        .iter()
        .map(|import| FObjectImport {
            class_package: renumber(import.class_package),
            class_name: renumber(import.class_name),
            object_name: renumber(import.object_name),
            ..import.clone()
        })
        .collect();
    let exports_table = package
        .exports
        .iter()
        .map(|export| FObjectExport {
            object_name: renumber(export.object_name),
            ..export.clone()
        })
        .collect();
    let mut splices = Vec::new();
    for at in name_offsets(parsed) {
        let index = index_at(exports, at, total)?;
        let now = remap[index as usize];
        if now != index {
            splices.push(Splice {
                start: at,
                end: at + 4,
                bytes: now.to_le_bytes().to_vec(),
            });
        }
    }
    Ok(Compaction {
        names: FPackageNameMap::create_from_names(kept),
        imports,
        exports: exports_table,
        splices,
    })
}

/// Which entries of the name map something uses: a name in export data, a name in the import or
/// export table, or the package's own name.
fn used_names(
    parsed: &ParsedPackage,
    package: &FLegacyPackageHeader,
    exports: &[u8],
    total: u64,
) -> Result<Vec<bool>, String> {
    let untracked: Vec<String> = parsed
        .exports
        .iter()
        .filter(|export| !export.names_complete)
        .map(|export| {
            let why = match &export.status {
                ExportStatus::Payload { kind, .. } => format!("holds {kind}"),
                ExportStatus::Partial { .. } => "does not read to its end".to_string(),
                ExportStatus::Failed { .. } => "does not read".to_string(),
                ExportStatus::Complete => "has bytecode that does not decode whole".to_string(),
            };
            format!("{} {why}", export.object_name)
        })
        .collect();
    if !untracked.is_empty() {
        let shown: Vec<&str> = untracked.iter().take(5).map(String::as_str).collect();
        let more = untracked.len() - shown.len();
        return Err(format!(
            "names can only be dropped from a package read whole, since unread bytes may hold \
             names nothing tracks: {}{}",
            shown.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ));
    }
    if !package.cell_imports.is_empty() || !package.cell_exports.is_empty() {
        return Err("a package with Verse cells cannot have its names dropped".into());
    }
    let count = package.name_map.num_names();
    let mut used = vec![false; count];
    let mut mark = |index: i32, what: &dyn Fn() -> String| match usize::try_from(index)
        .ok()
        .and_then(|at| used.get_mut(at))
    {
        Some(slot) => {
            *slot = true;
            Ok(())
        }
        None => Err(format!(
            "{} names entry {index}, past the {count} names",
            what()
        )),
    };
    for at in name_offsets(parsed) {
        mark(index_at(exports, at, total)?, &|| {
            format!("the name at {at:#X}")
        })?;
    }
    for (position, import) in package.imports.iter().enumerate() {
        for name in [import.class_package, import.class_name, import.object_name] {
            mark(name.index, &|| format!("import {position}"))?;
        }
    }
    for (position, export) in package.exports.iter().enumerate() {
        mark(export.object_name.index, &|| format!("export {position}"))?;
    }
    // The package's own name, which a map may hold whole or, like any name ending in `_N`, as the
    // part before the number.
    let own = &package.summary.package_name;
    for name in [own.as_str(), without_number(own)] {
        if let Some(at) = package
            .name_map
            .raw_names()
            .iter()
            .position(|held| held.eq_ignore_ascii_case(name))
        {
            used[at] = true;
        }
    }
    Ok(used)
}

/// A name without the `_N` an FName stores as its number: digits with no leading zero.
fn without_number(name: &str) -> &str {
    match name.rsplit_once('_') {
        Some((head, digits))
            if digits
                .parse::<i32>()
                .is_ok_and(|n| n >= 0 && n.to_string() == digits) =>
        {
            head
        }
        _ => name,
    }
}

/// Every recorded name in export data, once each, in file order.
fn name_offsets(parsed: &ParsedPackage) -> Vec<u64> {
    let mut offsets: Vec<u64> = parsed
        .exports
        .iter()
        .flat_map(|export| export.name_refs.iter().copied())
        .collect();
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

/// The name index stored at file offset `at`.
fn index_at(exports: &[u8], at: u64, total: u64) -> Result<i32, String> {
    let start = at
        .checked_sub(total)
        .and_then(|local| usize::try_from(local).ok())
        .ok_or_else(|| format!("a name is recorded at {at:#X}, inside the package header"))?;
    let bytes = exports
        .get(start..start + 4)
        .ok_or_else(|| format!("a name is recorded at {at:#X}, past the export data"))?;
    Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}
