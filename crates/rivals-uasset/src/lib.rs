//! Decodes Unreal Engine 5 package export data into readable property trees using a .usmap schema.
#![deny(clippy::unwrap_used, clippy::expect_used)]

mod copy;
mod datatable;
mod dependency;
mod duplicate;
mod edit;
mod export_edit;
mod header_edit;
mod hex;
mod import_remove;
mod kismet;
mod mappings;
mod moviescene;
mod niagara;
mod package;
mod props;
mod reader;
mod remove;
mod renumber;
mod stringtable;
mod structs;
mod tagged;
#[cfg(test)]
mod tagged_edit_tests;
mod tails;
mod unversioned;
mod ustruct;
mod value;
mod write;

pub use copy::{CopiedExport, CopyExport, CopyPlan, CopySource, plan_copy};
pub use datatable::{DataTable, DataTableLayout, DataTableRow, RowSpan};
pub use dependency::{DependencyEdit, DependencyPlan, Runs, plan_dependency_edits, runs_of};
pub use duplicate::{DuplicatePlan, LevelSlot, plan_duplication};
pub use edit::{
    AppliedEdit, BulkEdit, DRIFT, DuplicateExport, EditOp, Expected, KeyEdit, KeyOp, PackageEdits,
    PatchedBundle, PayloadEdit, RowEdit, RowOp, ScriptConstEdit, Sidecars, StringEdit, StringOp,
    ValueEdit, check_expectations, expectations, kind_of, patch_package, patch_package_copy,
    patch_package_with, patch_values, payload_lock, verify_copy, verify_patch,
};
pub use export_edit::{
    EDITABLE_FLAGS, ExportEdit, ExportEditPlan, flag_names, plan_export_edits,
    plan_export_edits_with,
};
pub use header_edit::{ImportEdit, ObjectPath, parse_object_path};
pub use hex::{HexRow, ROW_BYTES, render as render_hex, rows as hex_rows};
pub use import_remove::{
    ImportRemovalPlan, ImportUsage, RemovedImport, UnusedImport, import_usage, plan_import_removal,
    unused_imports,
};
pub use kismet::{
    Expr, LiteralSlot, ObjectRef, PropertyRef, Script, ScriptLine, ScriptStop, Statement,
    SwitchCase, Term, TermKind, TextLiteral, call_sites, callee_name, literals, render_script,
    script_lines, statement_terms, token_name, ubergraph_entries,
};
pub use mappings::{Mappings, Schema, SchemaFixups, SchemaSlot, kind_name};
pub use package::{
    AppliedFixup, AssetBundle, ExportStatus, ImportInfo, PackageInfo, ParseOptions, ParsedExport,
    ParsedPackage, TwinChoice, dotted_path, export_bytes, header_size, is_unresolved_import_name,
    lost_import_warning, package_info, package_names, parse_package, parse_package_checked,
    parse_package_opts, parse_package_probed, parse_package_traced, parse_package_traced_with,
    parse_package_with, read_header, unresolved_import_note, unresolved_imports,
};
pub use props::{
    ContainerLayout, Diagnostics, IndexRef, InstancedLayout, MapKeys, MissingSchema, TraceEntry,
    UndecodedPayload, UnsetSlot,
};
pub use remove::{ClearedReference, Importers, RemovalPlan, RemovedExport, plan_removal};
pub use stringtable::{StringEntrySpan, StringTable, StringTableEntry, StringTableLayout};
pub use ustruct::{
    FieldRole, FunctionField, FunctionSignature, StructDefinition, definitions_of,
    mappings_from_definitions, mappings_from_definitions_with, read_struct_definitions,
    read_struct_definitions_pathed,
};
pub use value::{MapEntry, PropertyEntry, PropertyValue};
pub use write::{
    HeaderDraft, IN_SEPARATE_FILE, InlinePayload, MEMORY_MAPPED, OPTIONAL_PAYLOAD, ResourceInfo,
    RewrittenPackage, SEPARATE_PAYLOAD_FLAGS, Splice, bulk_lock, header_round_trips,
    inline_payloads, locate_inline_payload, placement, resource_infos, rewrite,
};
