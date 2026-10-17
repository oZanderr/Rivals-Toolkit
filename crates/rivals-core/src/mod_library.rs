//! The mods folder as a whole: what is installed, in which state and order, which switched-on mods
//! carry the same files, and installing more.

mod conflicts;
mod install;
mod manage;
mod status;

use std::path::Path;

pub use conflicts::{
    AssetConflict, ConflictGroup, ConflictReport, ContentLister, DiskLister, check_conflicts,
};
pub use install::{
    Found, InstallOptions, InstallResult, NamedInstall, Packed, PackedFrom, UnpackedArchive,
    install, install_each, install_pak, install_source, unpack_archive,
};
pub use manage::{BulkFailure, BulkOpResult, delete_many, toggle, toggle_many};
#[cfg(feature = "archives")]
pub use manage::{archived_mod_count, export_archive};
pub use status::{ModKind, ModLibrary, ModStatus, list_mods, remove_stale_copies, size_on_disk};

/// The archive formats mods come in, by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    SevenZ,
    Rar,
}

pub fn archive_kind(path: &Path) -> Result<ArchiveKind, String> {
    match path
        .extension()
        .and_then(|x| x.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("zip") => Ok(ArchiveKind::Zip),
        Some("7z") => Ok(ArchiveKind::SevenZ),
        Some("rar") => Ok(ArchiveKind::Rar),
        _ => Err(format!("Unsupported archive format: {}", path.display())),
    }
}
