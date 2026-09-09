//! Renumbers package indices after the export or import table changes shape.
//!
//! Removing a row shifts every row after it, and an index is stored in five places at once: the
//! export table's own references, the import table's outers, the preload dependency runs, the bulk
//! data table, and the decoded references inside the export bytes. Getting one of them wrong
//! points a reference at whatever now sits at that number, which reads as a plausible object.

use std::collections::{BTreeMap, BTreeSet};

use retoc::legacy_asset::{
    FLegacyPackageHeader, FObjectDataResource, FObjectExport, FObjectImport,
};
use retoc::zen::FPackageIndex;

use crate::package::ParsedPackage;
use crate::write::Splice;

/// How the tables are changing, and therefore what each index becomes.
pub(crate) struct Renumber {
    removed_exports: BTreeSet<u32>,
    removed_imports: BTreeSet<u32>,
    /// Exports that gain a copy elsewhere in the table, by source position.
    copies: BTreeMap<u32, u32>,
}

impl Renumber {
    pub(crate) fn removing_exports(removed: BTreeSet<u32>) -> Self {
        Self {
            removed_exports: removed,
            removed_imports: BTreeSet::new(),
            copies: BTreeMap::new(),
        }
    }

    pub(crate) fn removing_imports(removed: BTreeSet<u32>) -> Self {
        Self {
            removed_exports: BTreeSet::new(),
            removed_imports: removed,
            copies: BTreeMap::new(),
        }
    }

    /// A copy set: an index inside it becomes the copy's, and nothing moves.
    pub(crate) fn copying(copies: BTreeMap<u32, u32>) -> Self {
        Self {
            removed_exports: BTreeSet::new(),
            removed_imports: BTreeSet::new(),
            copies,
        }
    }

    /// Whether this index names something that is going away, and so has to be cleared.
    pub(crate) fn drops(&self, index: FPackageIndex) -> bool {
        (index.is_export() && self.removed_exports.contains(&index.to_export_index()))
            || (index.is_import() && self.removed_imports.contains(&index.to_import_index()))
    }

    /// What an index becomes: null when its target goes, the copy's number inside a copy set,
    /// otherwise itself shifted down by whatever was removed below it.
    pub(crate) fn remap(&self, index: FPackageIndex) -> FPackageIndex {
        if self.drops(index) {
            return FPackageIndex::create_null();
        }
        if index.is_export() {
            let at = index.to_export_index();
            if let Some(&copy) = self.copies.get(&at) {
                return FPackageIndex::create_export(copy);
            }
            let shift = self
                .removed_exports
                .iter()
                .filter(|&&gone| gone < at)
                .count() as u32;
            return FPackageIndex::create_export(at - shift);
        }
        if index.is_import() {
            let at = index.to_import_index();
            let shift = self
                .removed_imports
                .iter()
                .filter(|&&gone| gone < at)
                .count() as u32;
            return FPackageIndex::create_import(at - shift);
        }
        index
    }

    /// Whether anything at all moves, so a caller can skip the work when nothing does.
    pub(crate) fn is_empty(&self) -> bool {
        self.removed_exports.is_empty() && self.removed_imports.is_empty() && self.copies.is_empty()
    }
}

/// Four-byte splices over every decoded reference whose value changes. `skip` names exports whose
/// bytes are going away, so a reference inside one is not worth rewriting.
pub(crate) fn reference_splices(
    parsed: &ParsedPackage,
    header: &FLegacyPackageHeader,
    renumber: &Renumber,
    skip: &BTreeSet<u32>,
) -> Vec<Splice> {
    let mut out: Vec<Splice> = Vec::new();
    if renumber.is_empty() {
        return out;
    }
    for reference in &parsed.references {
        let index = FPackageIndex {
            index: reference.index,
        };
        let now = renumber.remap(index);
        if now == index {
            continue;
        }
        let owner = header.exports.iter().position(|export| {
            let start = export.serial_offset.max(0) as u64;
            let end = start + export.serial_size.max(0) as u64;
            start <= reference.at && reference.at < end
        });
        if owner.is_none_or(|at| skip.contains(&(at as u32))) {
            continue;
        }
        out.push(Splice {
            start: reference.at,
            end: reference.at + 4,
            bytes: now.index.to_le_bytes().to_vec(),
        });
    }
    // A reference can be recorded twice when a parse rolled back over it, and two splices at one
    // offset would be an overlap.
    out.sort_by_key(|splice| splice.start);
    out.dedup_by_key(|splice| splice.start);
    out
}

/// Every index the header tables hold, renumbered in place.
pub(crate) fn remap_table(
    exports: &mut [FObjectExport],
    imports: &mut [FObjectImport],
    resources: &mut Vec<FObjectDataResource>,
    renumber: &Renumber,
) {
    for export in exports.iter_mut() {
        export.class_index = renumber.remap(export.class_index);
        export.super_index = renumber.remap(export.super_index);
        export.template_index = renumber.remap(export.template_index);
        export.outer_index = renumber.remap(export.outer_index);
    }
    for import in imports.iter_mut() {
        import.outer_index = renumber.remap(import.outer_index);
    }
    resources.retain(|resource| !renumber.drops(resource.outer_index));
    for resource in resources.iter_mut() {
        resource.outer_index = renumber.remap(resource.outer_index);
    }
}

/// The four preload dependency runs an export declares, read out of the shared table.
pub(crate) type Runs = [Vec<FPackageIndex>; 4];

/// Rebuilds the whole preload dependency table in export order, handing each export's runs to
/// `runs_for` and taking back what it should hold. The runs sit back to back and are addressed by
/// one index per export, so every one of them has to be written again whenever any changes.
///
/// `exports` may be longer than the header's, which is how appended copies get empty input runs.
pub(crate) fn rebuild_runs(
    header: &FLegacyPackageHeader,
    exports: &mut [FObjectExport],
    mut runs_for: impl FnMut(usize, Runs) -> Result<Runs, String>,
) -> Result<Vec<FPackageIndex>, String> {
    let mut table: Vec<FPackageIndex> = Vec::with_capacity(header.preload_dependencies.len());
    for (position, export) in exports.iter_mut().enumerate() {
        let first = export.first_export_dependency_index;
        let counts = [
            export.serialize_before_serialize_dependencies,
            export.create_before_serialize_dependencies,
            export.serialize_before_create_dependencies,
            export.create_before_create_dependencies,
        ];
        let mut cursor = usize::try_from(first).unwrap_or(0);
        let mut held: Runs = Default::default();
        // An export the caller appended has no runs in the table it came from.
        let readable = position < header.exports.len() && first >= 0;
        for (run, count) in held.iter_mut().zip(counts) {
            for _ in 0..usize::try_from(count).unwrap_or(0) {
                if !readable {
                    break;
                }
                let dep = header
                    .preload_dependencies
                    .get(cursor)
                    .copied()
                    .ok_or("the preload dependencies run past their table")?;
                cursor += 1;
                run.push(dep);
            }
        }
        let wanted = runs_for(position, held)?;
        let start = table.len() as i32;
        let mut written = 0usize;
        for run in &wanted {
            table.extend(run.iter().copied());
            written += run.len();
        }
        export.serialize_before_serialize_dependencies = wanted[0].len() as i32;
        export.create_before_serialize_dependencies = wanted[1].len() as i32;
        export.serialize_before_create_dependencies = wanted[2].len() as i32;
        export.create_before_create_dependencies = wanted[3].len() as i32;
        if first >= 0 || written > 0 {
            export.first_export_dependency_index = start;
        }
    }
    Ok(table)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn exports() -> BTreeSet<u32> {
        BTreeSet::from([1, 3])
    }

    /// A removed export becomes null, and everything above it moves down by however many went.
    #[test]
    fn removing_exports_clears_them_and_shifts_the_rest() {
        let renumber = Renumber::removing_exports(exports());
        assert!(renumber.remap(FPackageIndex::create_export(1)).is_null());
        assert!(renumber.remap(FPackageIndex::create_export(3)).is_null());
        assert_eq!(
            renumber.remap(FPackageIndex::create_export(0)),
            FPackageIndex::create_export(0)
        );
        assert_eq!(
            renumber.remap(FPackageIndex::create_export(2)),
            FPackageIndex::create_export(1)
        );
        assert_eq!(
            renumber.remap(FPackageIndex::create_export(4)),
            FPackageIndex::create_export(2)
        );
        // Imports are untouched by an export removal, and null stays null.
        assert_eq!(
            renumber.remap(FPackageIndex::create_import(2)),
            FPackageIndex::create_import(2)
        );
        assert!(renumber.remap(FPackageIndex::create_null()).is_null());
    }

    /// Removing an import shifts the imports and leaves the exports where they are, which is what
    /// makes the two removals separate operations rather than one.
    #[test]
    fn removing_imports_shifts_only_imports() {
        let renumber = Renumber::removing_imports(BTreeSet::from([0]));
        assert!(renumber.remap(FPackageIndex::create_import(0)).is_null());
        assert_eq!(
            renumber.remap(FPackageIndex::create_import(1)),
            FPackageIndex::create_import(0)
        );
        assert_eq!(
            renumber.remap(FPackageIndex::create_export(2)),
            FPackageIndex::create_export(2)
        );
    }

    /// A copy set points an index at the copy and moves nothing.
    #[test]
    fn copying_points_at_the_copy() {
        let renumber = Renumber::copying(BTreeMap::from([(1, 7)]));
        assert_eq!(
            renumber.remap(FPackageIndex::create_export(1)),
            FPackageIndex::create_export(7)
        );
        assert_eq!(
            renumber.remap(FPackageIndex::create_export(2)),
            FPackageIndex::create_export(2)
        );
    }

    fn export_with(first: i32, counts: [i32; 4]) -> FObjectExport {
        FObjectExport {
            first_export_dependency_index: first,
            serialize_before_serialize_dependencies: counts[0],
            create_before_serialize_dependencies: counts[1],
            serialize_before_create_dependencies: counts[2],
            create_before_create_dependencies: counts[3],
            ..Default::default()
        }
    }

    /// The runs are positional, so rebuilding one export's rewrites the whole table and every
    /// index into it.
    #[test]
    fn rebuilding_runs_rewrites_the_whole_table() {
        let header = FLegacyPackageHeader {
            exports: vec![export_with(0, [1, 0, 0, 0]), export_with(1, [0, 2, 0, 0])],
            preload_dependencies: vec![
                FPackageIndex::create_export(5),
                FPackageIndex::create_export(6),
                FPackageIndex::create_export(7),
            ],
            ..Default::default()
        };
        let mut exports = header.exports.clone();
        let table = rebuild_runs(&header, &mut exports, |position, mut runs| {
            if position == 0 {
                // Drop what the first export held.
                runs[0].clear();
            }
            Ok(runs)
        })
        .expect("rebuild");

        assert_eq!(table.len(), 2, "the first export's only dependency went");
        assert_eq!(exports[0].serialize_before_serialize_dependencies, 0);
        assert_eq!(exports[1].create_before_serialize_dependencies, 2);
        assert_eq!(
            exports[1].first_export_dependency_index, 0,
            "the second export's run moved down to where the first one's was"
        );
    }

    /// An export the caller appended is not in the table the runs were read from, so it starts
    /// with none rather than reading someone else's.
    #[test]
    fn an_appended_export_starts_with_no_runs() {
        let header = FLegacyPackageHeader {
            exports: vec![export_with(0, [1, 0, 0, 0])],
            preload_dependencies: vec![FPackageIndex::create_export(2)],
            ..Default::default()
        };
        let mut exports = header.exports.clone();
        exports.push(export_with(-1, [0, 0, 0, 0]));
        let table = rebuild_runs(&header, &mut exports, |_, mut runs| {
            runs[1].push(FPackageIndex::create_export(9));
            Ok(runs)
        })
        .expect("rebuild");

        assert_eq!(table.len(), 3, "one kept, and one added to each export");
        assert_eq!(exports[1].create_before_serialize_dependencies, 1);
        assert_eq!(exports[1].first_export_dependency_index, 2);
    }
}
