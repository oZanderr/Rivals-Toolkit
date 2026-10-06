//! A function's whole script written anew from assembler text, in a save.
//!
//! The text is assembled against the package as the save leaves it and spliced over the script.
//! Whatever outside the function pointed into it, an event stub's entry or another function's
//! latent resume point, follows the labels the text kept; one whose label is gone is refused,
//! naming what holds it. Before any of that, the script's own text has to assemble back to exactly
//! its bytes, which is what lets a text say everything the script does.

use std::collections::BTreeSet;

use crate::edit::{AppliedEdit, DRIFT, PackageEdits, ScriptTextEdit, bytes_at};
use crate::field_record::{NewField, encode_field_record, new_record, record_indices};
use crate::header_edit::Tables;
use crate::kismet::{self, Expr, Script};
use crate::package::{AssetBundle, ParsedExport, ParsedPackage};
use crate::relocate::{Change, OffsetMap, Relocated, relocate_through};
use crate::script_encode::{AssembleOptions, Assembled, Encoded, assemble, lay_out, moved_of};
use crate::script_text::{Diagnostic, original_offset, print_script};
use crate::write::Splice;

/// What one text edit writes, and what it moved outside its own script.
pub(crate) struct TextSplice {
    pub relocated: Relocated,
    pub applied: Vec<AppliedEdit>,
    pub links: Vec<(u64, i32)>,
    pub notes: Vec<String>,
}

/// How many of a text's problems one refusal lists.
const MAX_LISTED: usize = 20;

/// Text compared the way a reader would: line endings and trailing spaces do not count.
fn normalized(text: &str) -> String {
    text.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

fn script_of(parsed: &ParsedPackage, export: u32) -> Result<(&ParsedExport, &Script), String> {
    let found = parsed
        .exports
        .get(export as usize)
        .ok_or_else(|| format!("no export {export}"))?;
    let script = found
        .script
        .as_ref()
        .ok_or_else(|| format!("{} carries no bytecode", found.object_name))?;
    Ok((found, script))
}

/// Every problem a text has, as one refusal.
pub(crate) fn refusal(name: &str, errors: &[Diagnostic]) -> String {
    let mut listed: Vec<String> = errors
        .iter()
        .take(MAX_LISTED)
        .map(ToString::to_string)
        .collect();
    if errors.len() > MAX_LISTED {
        listed.push(format!("and {} more", errors.len() - MAX_LISTED));
    }
    format!(
        "the text for {name} does not assemble: {}",
        listed.join("; ")
    )
}

/// The labels a script prints with, by the offsets they were named after, and its end.
fn printed_labels(parsed: &ParsedPackage, export: u32, script: &Script) -> BTreeSet<u32> {
    let mut out: BTreeSet<u32> = print_script(parsed, export)
        .map(|text| {
            text.lines
                .iter()
                .flat_map(|line| line.labels.iter())
                .chain(&text.end_labels)
                .filter_map(|label| original_offset(&label.name))
                .collect()
        })
        .unwrap_or_default();
    out.insert(script.decoded_size);
    out
}

/// Works out one text edit: the script it writes, and everything outside it that moves.
pub(crate) fn plan_text(
    parsed: &ParsedPackage,
    edits: &PackageEdits,
    edit: &ScriptTextEdit,
    tables: &mut Tables,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    bundle: &AssetBundle<'_>,
    base: u64,
) -> Result<TextSplice, String> {
    let (found, script) = script_of(parsed, edit.export)?;
    let name = found.object_name.as_str();
    if let Some(stop) = &script.stopped {
        return Err(format!(
            "{name}'s script stops decoding at 0x{:04X} ({}), so it has no text to write it from",
            stop.offset, stop.reason
        ));
    }
    if edits.payloads.iter().any(|p| p.export == edit.export) {
        return Err(format!(
            "{name} is given a whole new payload in this save, so its script cannot also be written from text"
        ));
    }
    if edits.scripts.iter().any(|s| s.export == edit.export) {
        return Err(format!(
            "{name} is written from text in this save, so nothing inside its script can also be set"
        ));
    }
    if edits
        .script_texts
        .iter()
        .filter(|other| other.export == edit.export)
        .count()
        > 1
    {
        return Err(format!("{name} is written from two texts in one save"));
    }
    let printed = print_script(parsed, edit.export)
        .ok_or_else(|| format!("{name} carries no bytecode"))?
        .text();
    if !edits.allow_drift
        && let Some(was) = &edit.was
        && normalized(was) != normalized(&printed)
    {
        return Err(format!(
            "{DRIFT}: {name} prints otherwise than when this text was made. Re-read the asset and make the edit again, or apply it anyway"
        ));
    }
    // A script whose text does not give back its own bytes holds something the text leaves out,
    // which a text edit would quietly change.
    let original = bytes_at(bundle, base, script.start, script.end)?;
    let mut scratch = tables.clone();
    let back = assemble(
        &printed,
        parsed,
        edit.export,
        package,
        &mut scratch,
        AssembleOptions::default(),
    );
    if !back.is_ok_and(|back| back.bytes == original) {
        return Err(format!(
            "{name} does not come back byte for byte from its own text, so a text could change what it does not say. Change it with script-set instead"
        ));
    }
    let assembled = assemble(
        &edit.text,
        parsed,
        edit.export,
        package,
        tables,
        AssembleOptions {
            add_names: true,
            add_imports: true,
        },
    )
    .map_err(|errors| refusal(name, &errors))?;
    let labels = printed_labels(parsed, edit.export, script);
    if let Some(old) = assembled.moved.keys().find(|old| !labels.contains(old)) {
        return Err(format!(
            "{name}: @{old:04X} names no place in its script that anything enters or jumps to. A label of four or more hexadecimal digits stands for an offset the script was printed with; name one of your own with a word"
        ));
    }
    let map = match script.resize_lock() {
        Some(lock) => {
            let offsets: Vec<u32> = script.statements.iter().map(|s| s.offset).collect();
            let stored = (script.end - script.start) as usize;
            if assembled.loaded_size != script.buffer_size
                || assembled.bytes.len() != stored
                || assembled.statements != offsets
            {
                return Err(format!(
                    "{name} has to keep its layout ({lock}), so its text can change what each statement does but not where any starts or how long the script is"
                ));
            }
            OffsetMap::default()
        }
        None => OffsetMap::from_labels(assembled.moved.clone()),
    };
    check_entries(parsed, edit.export, name, &assembled)?;
    let statements = assembled.statements.len();
    let change = Change {
        at: script.start,
        end_at: script.end,
        offset: 0,
        end_offset: script.buffer_size,
        bytes: assembled.bytes.clone(),
        loaded_len: assembled.loaded_size,
        label: format!("{name}'s script"),
    };
    let mut relocated = relocate_through(parsed, edit.export, vec![change], map)?;
    let mut links: Vec<(u64, i32)> = assembled
        .links
        .iter()
        .map(|(at, index)| (script.start + at, *index))
        .collect();
    let mut applied = vec![AppliedEdit {
        name: format!("{name} script text"),
        offset: script.start,
        offset_after: script.start,
        element: None,
        elements_after: None,
        before: format!("{} statement(s)", script.statements.len()),
        after: format!("{statements} statement(s)"),
    }];
    for (what, old, new) in &relocated.moved {
        applied.push(AppliedEdit {
            name: what.clone(),
            offset: script.start,
            offset_after: script.start,
            element: None,
            elements_after: None,
            before: format!("0x{old:04X}"),
            after: format!("0x{new:04X}"),
        });
    }
    add_locals(
        found,
        &assembled,
        tables,
        &mut relocated,
        &mut links,
        &mut applied,
    )?;
    Ok(TextSplice {
        links,
        notes: assembled
            .warnings
            .iter()
            .map(|warning| format!("{name}: {warning}"))
            .collect(),
        relocated,
        applied,
    })
}

/// Gives the function the locals its text declares: a record for each after the ones it has, so
/// its parameters stay first, and the count of its fields grown to match. Whatever a record names
/// is imported, and the function takes a dependency on it.
fn add_locals(
    found: &ParsedExport,
    assembled: &Assembled,
    tables: &mut Tables,
    relocated: &mut Relocated,
    links: &mut Vec<(u64, i32)>,
    applied: &mut Vec<AppliedEdit>,
) -> Result<(), String> {
    if assembled.locals.is_empty() {
        return Ok(());
    }
    let name = found.object_name.as_str();
    let layout = found.layout.as_ref().ok_or_else(|| {
        format!("{name}'s fields were not read whole, so it can take no new local")
    })?;
    let mut bytes = Vec::new();
    for local in &assembled.locals {
        let record = new_record(&local.name, &local.ty, NewField::Local, tables)
            .map_err(|reason| format!("{name}, line {}: {reason}", local.pos.line))?;
        let mut indices = Vec::new();
        record_indices(&record, &mut indices);
        links.extend(indices.into_iter().map(|index| (layout.records_end, index)));
        bytes.extend(encode_field_record(&record, &mut tables.names));
        applied.push(AppliedEdit {
            name: format!("{name} local {}", local.name),
            offset: layout.records_end,
            offset_after: layout.records_end,
            element: None,
            elements_after: None,
            before: "(none)".into(),
            after: local.ty.printed(),
        });
    }
    let count = i32::try_from(layout.records.len() + assembled.locals.len())
        .map_err(|_| format!("{name} would hold too many fields"))?;
    relocated.splices.push((
        Splice {
            start: layout.child_properties_at,
            end: layout.child_properties_at + 4,
            bytes: count.to_le_bytes().to_vec(),
        },
        format!("{name}'s field count"),
    ));
    relocated.splices.push((
        Splice {
            start: layout.records_end,
            end: layout.records_end,
            bytes,
        },
        format!("{name}'s new locals"),
    ));
    Ok(())
}

/// An event stub a text writes enters its event graph where code starts: an entry landing inside
/// an expression would run whatever bytes it lands on.
fn check_entries(
    parsed: &ParsedPackage,
    export: u32,
    name: &str,
    assembled: &Assembled,
) -> Result<(), String> {
    let graphs: Vec<(i32, &str, &Script)> = parsed
        .exports
        .iter()
        .filter(|e| e.object_name.starts_with("ExecuteUbergraph"))
        .filter_map(|e| {
            let script = e.script.as_ref().filter(|script| script.complete())?;
            Some((e.index as i32 + 1, e.object_name.as_str(), script))
        })
        .collect();
    let mut failure = None;
    for root in &assembled.expressions {
        visit(root, &mut |expr| {
            let (graph, params) = match expr {
                Expr::FinalCall {
                    function, params, ..
                } => (
                    graphs.iter().find(|(index, _, _)| *index == function.index),
                    params,
                ),
                Expr::VirtualCall {
                    function, params, ..
                } => (
                    graphs.iter().find(|(_, graph, _)| graph == function),
                    params,
                ),
                _ => return,
            };
            let (Some((index, graph, script)), [only]) = (graph, params.as_slice()) else {
                return;
            };
            let Some(entry) = kismet::entry_value(only) else {
                return;
            };
            let lands = if *index == export as i32 + 1 {
                assembled.statements.contains(&entry)
            } else {
                script.spans.iter().any(|span| span.offset == entry)
            };
            if !lands && failure.is_none() {
                failure = Some(format!(
                    "{name} enters {graph} at 0x{entry:04X}, where no code of it starts"
                ));
            }
        });
    }
    failure.map_or(Ok(()), Err)
}

fn visit(expr: &Expr, each: &mut impl FnMut(&Expr)) {
    each(expr);
    for child in kismet::children(expr) {
        visit(child, each);
    }
}

/// A text edit's script read back against the package as saved: its statements, its code, and
/// where the offsets it had land now.
pub(crate) struct LaidOut {
    pub export: u32,
    pub expressions: Vec<Expr>,
    pub encoded: Encoded,
    pub map: OffsetMap,
}

/// Every text edit laid out against the saved package, which verification holds each saved
/// script, and every offset other functions hold into it, to.
pub(crate) fn lay_out_texts(
    before: &ParsedPackage,
    after: &ParsedPackage,
    edits: &PackageEdits,
) -> Result<Vec<LaidOut>, String> {
    let mut out = Vec::with_capacity(edits.script_texts.len());
    for edit in &edits.script_texts {
        let (found, script) = script_of(before, edit.export)?;
        let (expressions, encoded) = lay_out(&edit.text, after, edit.export).map_err(|errors| {
            format!(
                "{} after patching: {}",
                found.object_name,
                refusal(&found.object_name, &errors)
            )
        })?;
        let map = if script.resize_lock().is_some() {
            OffsetMap::default()
        } else {
            OffsetMap::from_labels(moved_of(&encoded, before, edit.export))
        };
        out.push(LaidOut {
            export: edit.export,
            expressions,
            encoded,
            map,
        });
    }
    Ok(out)
}

/// Holds a script written from text to what its text lays out as.
pub(crate) fn verify_text(name: &str, laid: &LaidOut, new: &Script) -> Result<(), String> {
    let stored = laid.encoded.bytes.len() as u32;
    if (new.buffer_size, new.storage_size) != (laid.encoded.loaded, stored) {
        return Err(format!(
            "{name}'s script is {} bytes loaded and {} stored after patching, where its text takes {} and {stored}",
            new.buffer_size, new.storage_size, laid.encoded.loaded
        ));
    }
    let offsets: Vec<u32> = new.statements.iter().map(|s| s.offset).collect();
    if offsets != laid.encoded.statements {
        return Err(format!(
            "{name}'s statements start elsewhere after patching than its text puts them"
        ));
    }
    for (expr, statement) in laid.expressions.iter().zip(&new.statements) {
        if kismet::shape(expr) != kismet::shape(&statement.expr) {
            return Err(format!(
                "{name} at 0x{:04X} reads {} after patching, where its text says {}",
                statement.offset,
                kismet::render(&statement.expr),
                kismet::render(expr)
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::edit::{ScriptConstEdit, Sidecars, patch_package_with, verify_patch};
    use crate::package::parse_package;
    use crate::script_fixture::{Built, event_graph};

    /// The event graph and its stub, as [`event_graph`] lays them out after their class.
    const GRAPH: u32 = 1;
    const STUB: u32 = 2;

    fn print(parsed: &ParsedPackage, export: u32) -> String {
        print_script(parsed, export).expect("a script").text()
    }

    struct Saved {
        before: ParsedPackage,
        after: ParsedPackage,
        applied: Vec<AppliedEdit>,
        exports: Vec<u8>,
    }

    /// Saves `edits` over the package, reads it back and verifies it, the way a save does.
    fn save(built: &Built, edits: &PackageEdits) -> Result<Saved, String> {
        let before = built.parsed();
        let patched =
            patch_package_with(&built.bundle(), Sidecars::default(), &before, edits, None)?;
        let after = parse_package(
            &AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            None,
        )?;
        verify_patch(&before, &after, edits, &patched.applied)?;
        Ok(Saved {
            before,
            after,
            applied: patched.applied,
            exports: patched.exports,
        })
    }

    fn text(export: u32, text: String) -> PackageEdits {
        PackageEdits {
            script_texts: vec![ScriptTextEdit {
                export,
                text,
                was: None,
            }],
            ..Default::default()
        }
    }

    /// The names and types of a function's locals, in record order.
    fn locals_of(parsed: &ParsedPackage, export: u32) -> Vec<(String, String)> {
        parsed.exports[export as usize]
            .signature
            .as_ref()
            .expect("its fields are read")
            .locals
            .iter()
            .map(|local| (local.name.clone(), local.kind.clone()))
            .collect()
    }

    /// A declared local is given to the function after the fields it has, so its parameters stay
    /// first, and the text can name it like any other.
    #[test]
    fn a_declared_local_is_given_to_the_function_and_named_by_the_text() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let grown = format!(
            "local Total: Int
local Seen: Array<Name>  ; names met so far
{}",
            printed.replacen(
                "Jump LocalVariable(EntryPoint)
",
                "Jump LocalVariable(EntryPoint)
Let LocalVariable(Total) = 6
",
                1,
            )
        );
        let saved = save(&built, &text(GRAPH, grown)).expect("saved");
        let pair = |name: &str, kind: &str| (name.to_string(), kind.to_string());
        assert_eq!(
            locals_of(&saved.after, GRAPH),
            [
                pair("Flag", "Bool"),
                pair("Count", "Int"),
                pair("Total", "Int"),
                pair("Seen", "Array<Name>"),
            ]
        );
        let params = |parsed: &ParsedPackage| -> Vec<String> {
            parsed.exports[GRAPH as usize]
                .signature
                .as_ref()
                .expect("its fields are read")
                .params
                .iter()
                .map(|param| param.name.clone())
                .collect()
        };
        assert_eq!(params(&saved.after), params(&saved.before));
        let graph = print(&saved.after, GRAPH);
        assert!(graph.contains("Let LocalVariable(Total) = 6"), "{graph}");
        assert!(
            saved.applied.iter().any(|applied| applied.name
                == "ExecuteUbergraph_BP_Test local Total"
                && applied.after == "Int"),
            "{:?}",
            saved.applied
        );
    }

    /// A conversion has the type it converts to and a choice the type its results share, so what
    /// either stores is held to where it goes. A choice between unlike results is not judged.
    #[test]
    fn a_conversion_or_a_choice_stored_in_another_type_is_found() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let lines = [
            "Let LocalVariable(Count) = Cast<DoubleToFloat>(LocalVariable(Amount))",
            "Let LocalVariable(Count) = SwitchValue(LocalVariable(Flag), True => 1.0f, default => 2.0f)",
            "Let LocalVariable(Count) = SwitchValue(LocalVariable(Flag), True => 1, default => 2.0f)",
        ];
        let stored = format!(
            "local Amount: Double\n{}",
            printed.replacen(
                "Jump LocalVariable(EntryPoint)\n",
                &format!("Jump LocalVariable(EntryPoint)\n{}\n", lines.join("\n")),
                1,
            )
        );
        let saved = save(&built, &text(GRAPH, stored)).expect("saved");
        let found: Vec<String> = crate::call_shape::stores_in(&saved.after, GRAPH, None)
            .into_iter()
            .map(|(_, fit)| format!("{fit:?}"))
            .collect();
        assert_eq!(
            found,
            [
                "Mismatch(\"stores a Float in Count, which is a Int\")",
                "Mismatch(\"stores a Float in Count, which is a Int\")",
            ],
        );
    }

    /// A local the function has already, declared with its own type, changes nothing.
    #[test]
    fn declaring_a_local_the_function_has_changes_nothing() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let saved = save(
            &built,
            &text(
                GRAPH,
                format!(
                    "local Count: Int
{printed}"
                ),
            ),
        )
        .expect("saved");
        assert_eq!(saved.exports, built.exports);
    }

    #[test]
    fn a_declaration_that_clashes_or_comes_late_is_refused() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let refused = |text: String| {
            save(&built, &self::text(GRAPH, text))
                .err()
                .expect("refused")
        };
        let parameter = refused(format!(
            "local EntryPoint: Int
{printed}"
        ));
        assert!(
            parameter.contains("line 1:1: EntryPoint is a parameter of ExecuteUbergraph_BP_Test"),
            "{parameter}"
        );
        let retyped = refused(format!(
            "local Count: Float
{printed}"
        ));
        assert!(
            retyped.contains("Count is already a local of ExecuteUbergraph_BP_Test, of type Int"),
            "{retyped}"
        );
        let late = refused(format!(
            "{printed}
local Late: Int"
        ));
        assert!(
            late.contains("declare a local before the first statement"),
            "{late}"
        );
        let unknown = refused(format!(
            "local Where: Vector
{printed}"
        ));
        assert!(unknown.contains("not a type"), "{unknown}");
    }

    /// Naming the function as a local's owner does not get round the function having to hold it.
    #[test]
    fn a_local_the_function_lacks_is_refused_with_its_owner_written() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let invented = printed.replacen(
            "Jump LocalVariable(EntryPoint)
",
            "Jump LocalVariable(EntryPoint)
Let LocalVariable(Nope in /Game/Test.BP_Test_C:ExecuteUbergraph_BP_Test) = 6
",
            1,
        );
        let refused = save(&built, &text(GRAPH, invented)).err().expect("refused");
        assert!(
            refused.contains("Nope is not a parameter or local of ExecuteUbergraph_BP_Test"),
            "{refused}"
        );
    }

    #[test]
    fn an_unchanged_text_saves_the_same_bytes() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let saved = save(&built, &text(GRAPH, printed)).expect("saved");
        assert_eq!(saved.exports, built.exports);
    }

    /// A statement added before the place the stub enters at moves the entry, the branch past it
    /// and the latent action's resume point, all by the statement's loaded length.
    #[test]
    fn a_text_that_grows_moves_what_its_labels_keep() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let grown = printed.replacen(
            "Jump LocalVariable(EntryPoint)\n",
            "Jump LocalVariable(EntryPoint)\nLet LocalVariable(Count) = 6\n",
            1,
        );
        let saved = save(&built, &text(GRAPH, grown)).expect("saved");
        let stub = print(&saved.after, STUB);
        assert!(
            stub.starts_with(
                "LocalFinalFunction /Game/Test.BP_Test_C:ExecuteUbergraph_BP_Test(33)"
            ),
            "{stub}"
        );
        let graph = print(&saved.after, GRAPH);
        assert!(
            graph.contains("@0021:  ; ReceiveBeginPlay enters here"),
            "{graph}"
        );
        assert!(graph.contains("Jump @00A9 unless"), "{graph}");
        assert!(graph.contains("SkipOffsetConst(@00A9)"), "{graph}");
        let moved = saved
            .applied
            .iter()
            .find(|applied| {
                applied
                    .name
                    .starts_with("the offset ReceiveBeginPlay enters")
            })
            .expect("the entry is reported");
        assert_eq!(
            (moved.before.as_str(), moved.after.as_str()),
            ("0x000A", "0x0021")
        );
        assert!(
            saved
                .applied
                .iter()
                .any(|applied| applied.name == "ExecuteUbergraph_BP_Test script layout"),
            "{:?}",
            saved.applied
        );
        assert_eq!(saved.before.exports.len(), saved.after.exports.len());
    }

    #[test]
    fn a_text_dropping_the_label_an_event_enters_at_names_the_stub() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let dropped = printed.replace("@000A:  ; ReceiveBeginPlay enters here\n", "");
        let refused = save(&built, &text(GRAPH, dropped)).err().expect("refused");
        assert!(
            refused.contains("ReceiveBeginPlay enters ExecuteUbergraph_BP_Test at 0x000A"),
            "{refused}"
        );
        assert!(refused.contains("no label @000A"), "{refused}");
    }

    #[test]
    fn a_label_the_script_never_had_is_refused() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let invented = printed.replacen(
            "Let LocalVariable(Count) = 5\n",
            "@0018:\nLet LocalVariable(Count) = 5\n",
            1,
        );
        let refused = save(&built, &text(GRAPH, invented)).err().expect("refused");
        assert!(refused.contains("@0018 names no place"), "{refused}");
    }

    #[test]
    fn a_stale_text_is_drift_unless_allowed() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let mut edits = text(GRAPH, printed.clone());
        edits.script_texts[0].was = Some("Return Nothing\nEndOfScript\n".into());
        let refused = save(&built, &edits).err().expect("refused");
        assert!(refused.starts_with(DRIFT), "{refused}");
        edits.allow_drift = true;
        save(&built, &edits).expect("saved anyway");
        // The text it printed, line endings aside, is no drift at all.
        edits.allow_drift = false;
        edits.script_texts[0].was = Some(printed.replace('\n', "\r\n"));
        save(&built, &edits).expect("saved");
    }

    #[test]
    fn a_text_and_another_edit_of_one_script_are_refused() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), GRAPH);
        let mut edits = text(GRAPH, printed.clone());
        edits.scripts.push(ScriptConstEdit {
            export: GRAPH,
            statement: 0x18,
            value: "7".into(),
            ..Default::default()
        });
        let refused = save(&built, &edits).err().expect("refused");
        assert!(refused.contains("written from text"), "{refused}");
        let mut twice = text(GRAPH, printed.clone());
        twice.script_texts.push(ScriptTextEdit {
            export: GRAPH,
            text: printed,
            was: None,
        });
        let refused = save(&built, &twice).err().expect("refused");
        assert!(refused.contains("two texts"), "{refused}");
    }

    /// A text can name what the package never did: the name and the import are added in the same
    /// save, and the saved script reads back naming them.
    #[test]
    fn a_text_naming_a_new_name_and_object_grows_the_tables() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), STUB);
        let added = printed.replacen(
            "Return Nothing\n",
            "VirtualFunction Brand_New()\nCallMath /Script/Engine.KismetMathLibrary:Add_IntInt(1, 2)\nReturn Nothing\n",
            1,
        );
        let saved = save(&built, &text(STUB, added)).expect("saved");
        let stub = print(&saved.after, STUB);
        assert!(stub.contains("VirtualFunction Brand_New()"), "{stub}");
        assert!(
            stub.contains("CallMath /Script/Engine.KismetMathLibrary:Add_IntInt(1, 2)"),
            "{stub}"
        );
        assert!(saved.after.names.len() > saved.before.names.len());
        assert!(saved.after.imports.len() > saved.before.imports.len());
    }

    /// A script something points into in a way nothing can follow keeps its layout: its text can
    /// change a statement in place, but not move one.
    #[test]
    fn a_locked_script_takes_only_a_text_that_keeps_its_layout() {
        let mut package = event_graph();
        // A computed jump on a local other than the entry point goes wherever the local says.
        let flag = package.index_of("Flag");
        package.functions[0].script[6..10].copy_from_slice(&flag.to_le_bytes());
        let built = package.build();
        let parsed = built.parsed();
        let lock = parsed.exports[GRAPH as usize]
            .script
            .as_ref()
            .and_then(|script| script.resize_lock());
        assert!(lock.is_some_and(|lock| lock.contains("computed jump")));
        let printed = print(&parsed, GRAPH);
        let same_length = printed.replacen(
            "Let LocalVariable(Count) = 5\n",
            "Let LocalVariable(Count) = 9\n",
            1,
        );
        let saved = save(&built, &text(GRAPH, same_length)).expect("saved");
        assert!(print(&saved.after, GRAPH).contains("Let LocalVariable(Count) = 9"));
        let grown = printed.replacen(
            "Let LocalVariable(Count) = 5\n",
            "Let LocalVariable(Count) = 5\nLet LocalVariable(Count) = 6\n",
            1,
        );
        let refused = save(&built, &text(GRAPH, grown)).err().expect("refused");
        assert!(refused.contains("has to keep its layout"), "{refused}");
    }

    #[test]
    fn a_stub_entering_where_no_code_starts_is_refused() {
        let built = event_graph().build();
        let printed = print(&built.parsed(), STUB);
        let stray = printed.replacen(
            "ExecuteUbergraph_BP_Test(10)",
            "ExecuteUbergraph_BP_Test(11)",
            1,
        );
        let refused = save(&built, &text(STUB, stray)).err().expect("refused");
        assert!(
            refused
                .contains("enters ExecuteUbergraph_BP_Test at 0x000B, where no code of it starts"),
            "{refused}"
        );
    }

    #[test]
    fn a_text_that_does_not_assemble_says_where() {
        let built = event_graph().build();
        let refused = save(
            &built,
            &text(STUB, "LocalVirtualFunction Nowhere(\nEndOfScript\n".into()),
        )
        .err()
        .expect("refused");
        assert!(
            refused.contains("the text for ReceiveBeginPlay does not assemble: line"),
            "{refused}"
        );
    }
}
