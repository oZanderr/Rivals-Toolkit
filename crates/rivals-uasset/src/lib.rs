//! Decodes Unreal Engine 5 package export data into readable property trees using a .usmap schema.
#![deny(clippy::unwrap_used, clippy::expect_used)]

mod call_shape;
mod class_variable;
mod component;
mod copy;
mod datatable;
mod dependency;
mod duplicate;
mod edit;
mod export_edit;
mod field_record;
mod header_edit;
mod hex;
mod identity;
mod import_remove;
mod kismet;
mod mappings;
mod moviescene;
mod named_edit;
mod names;
mod new_function;
mod niagara;
mod package;
mod path_edit;
mod props;
mod reader;
mod relocate;
mod remove;
mod renumber;
mod script_edit;
mod script_encode;
#[cfg(test)]
mod script_fixture;
mod script_text;
mod script_text_edit;
pub use call_shape::{
    CallShape, CallSite, Fit, compare as compare_calls, render as render_call_shape,
    shape_of_signature, site_at, stores_in,
};
pub use script_edit::{narrowing_edits, widening_edits};
pub use script_encode::{
    AssembleOptions, Assembled, RoundTripFailure, RoundTrips, script_round_trips, text_diagnostics,
};
pub use script_text::{
    Diagnostic, ScriptPrinter, ScriptText, TextLabel, TextLine, print_expr, print_script,
};
mod stringtable;
mod structs;
mod tagged;
#[cfg(test)]
mod tagged_edit_tests;
#[cfg(any(test, feature = "test-support"))]
pub mod tagged_fixture;
mod tails;
pub mod text_literal;
mod unversioned;
#[cfg(test)]
mod unversioned_edit_tests;
#[cfg(test)]
mod unversioned_fixture;
mod ustruct;
mod value;
mod write;

pub use class_variable::AddVariable;
pub use component::{
    AddComponent, ChangedList, ComponentPlan, ComponentRemoval, InheritedComponent, ListContext,
    ListEntry, ListScope, NodeParent, RemoveComponent, component_removal_wiring, component_wiring,
    inherited_component_wiring, plan_component, plan_component_removal, plan_inherited_component,
    verify_component, verify_component_removal, verify_inherited_component,
};
pub use copy::{CopiedExport, CopyExport, CopyPlan, CopySource, plan_copy};
pub use datatable::{DataTable, DataTableLayout, DataTableRow, RowSpan};
pub use dependency::{
    DependencyEdit, DependencyPlan, Runs, plan_dependency_edits, runs_of, zen_keeps, zen_losses,
    zen_readback,
};
pub use duplicate::{
    AddExport, ClassLayout, DuplicatePlan, LevelSlot, class_layout, plan_duplication,
};
pub use edit::{
    AppliedEdit, BulkEdit, DRIFT, DuplicateExport, EditOp, Expected, FieldSet, KeyEdit, KeyOp,
    NOT_STORED, PackageEdits, PatchedBundle, PayloadEdit, RowEdit, RowOp, ScriptConstEdit,
    ScriptTextEdit, Sidecars, StringEdit, StringOp, ValueEdit, check_expectations, entry_named_at,
    expectations, kind_of, patch_identity, patch_package, patch_package_copy, patch_package_with,
    patch_values, payload_lock, same_enumerator, verify_copy, verify_identity, verify_patch,
    verify_references,
};
pub use export_edit::{
    EDITABLE_FLAGS, ExportEdit, ExportEditPlan, flag_names, plan_export_edits,
    plan_export_edits_with,
};
pub use field_record::{FieldType, NewField, encode_field_record, parse_field_type};
pub use header_edit::{ImportEdit, ObjectPath, parse_object_path};
pub use hex::{HexRow, ROW_BYTES, render as render_hex, rows as hex_rows};
pub use identity::{
    PathRename, SaveAs, identity_leftovers, identity_script_edits, identity_value_edits,
    names_renamed_paths, rename_references,
};
pub use import_remove::{
    ImportRemovalPlan, ImportUsage, RemovedImport, UnusedImport, import_usage, plan_import_removal,
    unused_imports,
};
pub use kismet::{
    CallInfo, CallUse, Expr, ExpressionSlot, LiteralSlot, ObjectRef, PropertyRef, ResizeLock,
    Script, ScriptCensus, ScriptLine, ScriptStop, SlotKind, Statement, SwitchCase, Term, TermKind,
    TextLiteral, call_at, call_expr_at, call_sites, callee_name, census as script_census,
    children as expression_children, expression_at, expression_starting, literals, render_script,
    script_lines, shape as expression_shape, statement_terms, token_name, ubergraph_entries,
};
pub use mappings::{Mappings, Schema, SchemaFixups, SchemaSlot, kind_name};
pub use named_edit::{names_any, place_named};
pub use names::unused_names;
pub use new_function::{NewFunctionEdit, new_function_export};
pub use package::{
    AppliedFixup, AssetBundle, ExportStatus, ImportInfo, PackageInfo, ParseOptions, ParsedExport,
    ParsedPackage, TwinChoice, dotted_path, export_bytes, header_size, is_unresolved_import_name,
    lost_import_warning, package_info, package_names, parse_package, parse_package_checked,
    parse_package_opts, parse_package_probed, parse_package_traced, parse_package_traced_with,
    parse_package_with, read_header, unresolved_import_note, unresolved_imports,
};
pub use path_edit::{
    Lowered, PathEdit, PathOp, Place, Segment, below_package, element_segment, elements_of,
    export_of, format_path, lower_paths, parse_path, place_of, was_of,
};
pub use props::{
    ContainerLayout, Diagnostics, IndexRef, InstancedLayout, MapKeys, MissingSchema, OddHeader,
    PREVIEW_DEPTH, TYPE_FIELD, TraceEntry, UNDECODED_FIELD, UndecodedPayload, UnsetSlot,
};
pub use remove::{ClearedReference, Importers, RemovalPlan, RemovedExport, plan_removal};
pub use stringtable::{StringEntrySpan, StringTable, StringTableEntry, StringTableLayout};
pub use structs::stored_whole;
pub use ustruct::type_text as property_type_text;
pub use ustruct::{
    FieldRecord, FieldRole, FunctionField, FunctionSignature, RecordTail, StructDefinition,
    StructLayout, definitions_of, mappings_from_definitions, mappings_from_definitions_with,
    read_struct_definitions, read_struct_definitions_pathed,
};
pub use value::{MapEntry, PropertyEntry, PropertyValue};
pub use write::{
    HeaderDraft, IN_SEPARATE_FILE, InlinePayload, MEMORY_MAPPED, OPTIONAL_PAYLOAD, ResourceInfo,
    RewrittenPackage, SEPARATE_PAYLOAD_FLAGS, Splice, bulk_lock, header_round_trips,
    inline_payloads, locate_inline_payload, placement, resource_infos, rewrite,
};
