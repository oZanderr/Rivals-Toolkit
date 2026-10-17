//! Finds the event graph code only some events reach, and the graph's text without it.
//!
//! An event stub calls its class's event graph at an offset, and from there execution flows by
//! falling through and jumping. Removing a stub leaves the code only it entered unreachable, so
//! that code, and whatever it alone names, can go with it.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::kismet::{Expr, FixupKind, FixupSource, Script, calls, functions_of, inbound};
use crate::package::{AssetBundle, ParsedPackage};
use crate::script_text::{TextLabel, original_offset, print_script};

/// Whether execution can carry on to the next statement.
fn falls_through(expr: &Expr) -> bool {
    !matches!(
        expr,
        Expr::Return { .. }
            | Expr::Jump { .. }
            | Expr::ComputedJump { .. }
            | Expr::Simple {
                name: "EndOfScript" | "PopExecutionFlow"
            }
    )
}

/// The statement holding loaded offset `offset`: the one starting there, or the one a target in
/// the middle of a statement lands inside. Past the last statement is none.
fn holding(script: &Script, offset: u32) -> Option<usize> {
    if offset >= script.decoded_size {
        return None;
    }
    match script
        .statements
        .binary_search_by_key(&offset, |statement| statement.offset)
    {
        Ok(at) => Some(at),
        Err(0) => None,
        Err(at) => Some(at - 1),
    }
}

/// Where statement `at` sits in the file.
fn file_range(script: &Script, at: usize) -> (u64, u64) {
    let start = script.statements[at].at;
    let end = script
        .statements
        .get(at + 1)
        .map_or(script.end, |next| next.at);
    (start, end)
}

/// Which statements of `script`, a function named `own`, execution reaches from `roots`, loaded
/// offsets into it. A latent action resuming here is followed from the code that starts it; a
/// pushed flow is reached once the push is, which over-approximates the flow stack safely.
pub(crate) fn reachable(
    script: &Script,
    own: &str,
    roots: impl IntoIterator<Item = u32>,
) -> Vec<bool> {
    let count = script.statements.len();
    let mut seen = vec![false; count];
    let mut fixups: Vec<_> = script.fixups.iter().collect();
    fixups.sort_by_key(|fixup| fixup.at);
    let mut queue: Vec<usize> = roots
        .into_iter()
        .filter_map(|offset| holding(script, offset))
        .collect();
    while let Some(at) = queue.pop() {
        if seen[at] {
            continue;
        }
        seen[at] = true;
        let (start, end) = file_range(script, at);
        let first = fixups.partition_point(|fixup| fixup.at < start);
        for fixup in fixups[first..].iter().take_while(|fixup| fixup.at < end) {
            let FixupKind::Absolute { target, resumes } = &fixup.kind else {
                continue;
            };
            let follows = match fixup.source {
                FixupSource::Jump | FixupSource::JumpIfNot | FixupSource::PushFlow => true,
                FixupSource::Linkage => resumes.as_deref().is_none_or(|name| name == own),
                FixupSource::SwitchEnd | FixupSource::SwitchNext | FixupSource::ContextSkip => {
                    false
                }
            };
            if follows && let Some(next) = holding(script, *target) {
                queue.push(next);
            }
        }
        if falls_through(&script.statements[at].expr) && at + 1 < count {
            queue.push(at + 1);
        }
    }
    seen
}

/// A statement a trim takes out, as the text printed it.
#[derive(Debug, Clone, Serialize)]
pub struct DroppedLine {
    pub offset: u32,
    pub text: String,
}

/// The code of one event graph only the events going reach, and the graph's text without it.
#[derive(Debug, Clone, Serialize)]
pub struct GraphTrim {
    pub graph: u32,
    pub name: String,
    pub dropped: Vec<DroppedLine>,
    /// The events going whose entry sat in the code taken out: their labels move to the graph's
    /// last return, which is where each lands until its stub goes too.
    pub parked: Vec<String>,
    /// Where each statement taken out sits in the file, for telling a use inside it from a live one.
    #[serde(skip)]
    pub ranges: Vec<(u64, u64)>,
    /// The statements are assignments storing into what goes, rather than code only it reached.
    pub assignments: bool,
    #[serde(skip)]
    pub text: String,
}

/// The trims removing `going` makes, and why a graph they enter keeps its code when it does.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Trims {
    pub graphs: Vec<GraphTrim>,
    pub kept: Vec<String>,
}

/// The event graph code only the functions `going` enter. Code already unreachable stays, and so
/// does the dispatch, the last return and the end of the script. A graph whose layout cannot change,
/// or whose text does not assemble back to its bytes, keeps its code, with the reason in `kept`.
pub fn trim_event_graphs(bundle: &AssetBundle<'_>, parsed: &ParsedPackage, going: &[u32]) -> Trims {
    let mut out = Trims::default();
    let functions = functions_of(&parsed.exports);
    let opaque = parsed.exports.iter().any(|export| {
        export
            .script
            .as_ref()
            .is_some_and(|script| !script.complete())
    });
    for graph in functions
        .iter()
        .filter(|function| function.is_event_graph())
    {
        let into = inbound(&functions, graph);
        let leaving: BTreeSet<u32> = into
            .entries
            .iter()
            .filter(|entry| going.contains(&entry.export))
            .map(|entry| entry.offset)
            .collect();
        if leaving.is_empty() {
            continue;
        }
        let events: Vec<&str> = into
            .entries
            .iter()
            .filter(|entry| going.contains(&entry.export))
            .filter_map(|entry| parsed.exports.get(entry.export as usize))
            .map(|export| export.object_name.as_str())
            .collect();
        let mut keep = |why: String| {
            out.kept.push(format!(
                "the code only {} reach stays in {}: {why}",
                events.join(", "),
                graph.name
            ));
        };
        let script = graph.script;
        if opaque {
            keep("a script in the package does not decode whole, so what enters the graph is not known".into());
            continue;
        }
        if let Some(lock) = script.resize_lock() {
            keep(lock);
            continue;
        }
        if let Err(failure) = crate::script_encode::round_trips_one(bundle, parsed, graph.export) {
            keep(format!(
                "its text does not assemble back to its bytes ({})",
                failure.detail
            ));
            continue;
        }
        let Some(last_return) = script
            .statements
            .iter()
            .rposition(|statement| matches!(statement.expr, Expr::Return { .. }))
        else {
            keep("it has no return to land its removed entries on".into());
            continue;
        };
        let linkages = into
            .linkages
            .iter()
            .filter_map(|(_, fixup)| match fixup.kind {
                FixupKind::Absolute { target, .. } => Some(target),
                FixupKind::Relative { .. } => None,
            });
        let staying: Vec<u32> = into
            .entries
            .iter()
            .filter(|entry| !going.contains(&entry.export))
            .map(|entry| entry.offset)
            .chain(linkages.clone())
            .chain(std::iter::once(0))
            .collect();
        let before = reachable(
            script,
            graph.name,
            staying.iter().copied().chain(leaving.iter().copied()),
        );
        let after = reachable(script, graph.name, staying);
        let ends = |at: usize| {
            at == 0
                || at == last_return
                || matches!(
                    script.statements[at].expr,
                    Expr::Simple {
                        name: "EndOfScript"
                    }
                )
        };
        let dead: Vec<bool> = (0..before.len())
            .map(|at| before[at] && !after[at] && !ends(at))
            .collect();
        if !dead.contains(&true) {
            continue;
        }
        let Some(text) = print_script(parsed, graph.export) else {
            keep("its script does not print".into());
            continue;
        };
        if text.lines.len() != script.statements.len() {
            keep("its text does not hold a line per statement".into());
            continue;
        }
        let mut trim = GraphTrim {
            graph: graph.export,
            name: graph.name.to_string(),
            dropped: Vec::new(),
            parked: Vec::new(),
            ranges: Vec::new(),
            assignments: false,
            text: String::new(),
        };
        let mut parked: Vec<TextLabel> = Vec::new();
        let mut kept = text.clone();
        kept.lines.clear();
        let mut return_line = 0;
        for (at, line) in text.lines.into_iter().enumerate() {
            if !dead[at] {
                if at == last_return {
                    return_line = kept.lines.len();
                }
                kept.lines.push(line);
                continue;
            }
            for label in &line.labels {
                if original_offset(&label.name).is_some_and(|offset| leaving.contains(&offset)) {
                    parked.push(label.clone());
                }
            }
            trim.ranges.push(file_range(script, at));
            trim.dropped.push(DroppedLine {
                offset: line.offset,
                text: line.text,
            });
        }
        for label in &parked {
            if let Some(offset) = original_offset(&label.name) {
                trim.parked.extend(
                    into.entries
                        .iter()
                        .filter(|entry| entry.offset == offset && going.contains(&entry.export))
                        .filter_map(|entry| parsed.exports.get(entry.export as usize))
                        .map(|export| export.object_name.clone()),
                );
            }
        }
        kept.lines[return_line].labels.extend(parked);
        trim.text = kept.text();
        out.graphs.push(trim);
    }
    out
}

/// The text of function `export` without statements `going`: assignments storing into a variable
/// that goes with them, each refused unless its value calls nothing. A label on a dropped line
/// moves to the next line kept, which is where execution lands once the assignment is gone.
pub fn drop_assignments(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    export: u32,
    going: &BTreeSet<usize>,
) -> Result<GraphTrim, String> {
    let function = parsed
        .exports
        .get(export as usize)
        .ok_or_else(|| format!("this package has no export {export}"))?;
    let name = &function.object_name;
    let script = function
        .script
        .as_ref()
        .filter(|script| script.complete())
        .ok_or_else(|| format!("{name}'s script does not decode whole"))?;
    if let Some(lock) = script.resize_lock() {
        return Err(format!("{name} has to keep its layout: {lock}"));
    }
    if let Err(failure) = crate::script_encode::round_trips_one(bundle, parsed, export) {
        return Err(format!(
            "{name}'s text does not assemble back to its bytes ({})",
            failure.detail
        ));
    }
    let text =
        print_script(parsed, export).ok_or_else(|| format!("{name}'s script does not print"))?;
    if text.lines.len() != script.statements.len() {
        return Err(format!("{name}'s text does not hold a line per statement"));
    }
    for &at in going {
        let line = text
            .lines
            .get(at)
            .ok_or_else(|| format!("{name} has no statement {at}"))?;
        match &script.statements[at].expr {
            Expr::Let { value, .. } if !calls(value) => {}
            Expr::Let { .. } => {
                return Err(format!(
                    "{name}'s `{}` calls something, which would go with it: move the call out of \
                     the assignment first",
                    line.text
                ));
            }
            _ => return Err(format!("{name}'s `{}` is not an assignment", line.text)),
        }
    }
    let mut trim = GraphTrim {
        graph: export,
        name: name.clone(),
        dropped: Vec::new(),
        parked: Vec::new(),
        ranges: Vec::new(),
        assignments: true,
        text: String::new(),
    };
    let mut kept = text.clone();
    kept.lines.clear();
    let mut carried: Vec<TextLabel> = Vec::new();
    for (at, mut line) in text.lines.into_iter().enumerate() {
        if going.contains(&at) {
            carried.append(&mut line.labels);
            trim.ranges.push(file_range(script, at));
            trim.dropped.push(DroppedLine {
                offset: line.offset,
                text: line.text,
            });
            continue;
        }
        carried.append(&mut line.labels);
        line.labels = std::mem::take(&mut carried);
        kept.lines.push(line);
    }
    if !carried.is_empty() {
        return Err(format!(
            "{name} ends on an assignment, with nothing after it to land on"
        ));
    }
    trim.text = kept.text();
    Ok(trim)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::edit::{PackageEdits, ScriptTextEdit, Sidecars, patch_package_with, verify_patch};
    use crate::package::parse_package;
    use crate::script_fixture::{Built, event_graph, two_events_laid};

    fn text_edit(export: u32, text: &str) -> PackageEdits {
        PackageEdits {
            script_texts: vec![ScriptTextEdit {
                export,
                text: text.to_string(),
                was: None,
                object: None,
            }],
            ..Default::default()
        }
    }

    /// Only an assignment calling nothing is dropped this way: a call standing alone, or one whose
    /// value is computed by a call, would take what it does with it.
    #[test]
    fn only_an_assignment_calling_nothing_is_dropped() {
        let built = two_events_laid();
        let parsed = built.parsed();
        let script = parsed.exports[1].script.as_ref().expect("graph");
        let call = script
            .statements
            .iter()
            .position(|statement| matches!(statement.expr, Expr::FinalCall { .. }))
            .expect("the Delay");
        let refused = drop_assignments(&built.bundle(), &parsed, 1, &BTreeSet::from([call]))
            .expect_err("a call");
        assert!(refused.contains("is not an assignment"), "{refused}");

        let called = Expr::FinalCall {
            name: "CallMath",
            function: crate::kismet::ObjectRef {
                index: -1,
                path: None,
            },
            params: Vec::new(),
        };
        assert!(calls(&called));
        let cast = Expr::Skip {
            skip: 0,
            value: Box::new(called),
        };
        assert!(calls(&cast), "a call inside counts");
        assert!(!calls(&Expr::SelfRef));
    }

    fn dropped(trim: &GraphTrim) -> Vec<&str> {
        trim.dropped.iter().map(|line| line.text.as_str()).collect()
    }

    #[test]
    fn every_entry_reaches_all_but_the_dead_tail_and_none_reaches_only_the_dispatch() {
        let laid: Built = two_events_laid();
        let parsed = laid.parsed();
        let script = parsed.exports[1].script.as_ref().expect("a script");
        let functions = functions_of(&parsed.exports);
        let into = inbound(&functions, &functions[0]);
        assert_eq!(into.entries.len(), 2);
        let all = reachable(
            script,
            "ExecuteUbergraph_BP_Test",
            std::iter::once(0).chain(into.entries.iter().map(|entry| entry.offset)),
        );
        // Past the return: the dead statement, then the end of the script.
        let past_return = script.statements.len() - 2;
        for (at, reached) in all.iter().enumerate() {
            assert_eq!(*reached, at < past_return, "statement {at}");
        }
        let none = reachable(script, "ExecuteUbergraph_BP_Test", [0]);
        assert_eq!(none.iter().filter(|r| **r).count(), 1, "the dispatch alone");
    }

    #[test]
    fn a_jump_into_a_statement_keeps_the_statement_it_lands_in() {
        let laid: Built = two_events_laid();
        let parsed = laid.parsed();
        let script = parsed.exports[1].script.as_ref().expect("a script");
        let inside = script.statements[2].offset + 1;
        let reached = reachable(script, "ExecuteUbergraph_BP_Test", [inside]);
        assert!(reached[2]);
    }

    #[test]
    fn removing_tick_takes_its_delay_and_resume_and_keeps_the_shared_code() {
        let laid: Built = two_events_laid();
        let parsed = laid.parsed();
        let tick = parsed
            .exports
            .iter()
            .find(|e| e.object_name == "ReceiveTick")
            .expect("tick")
            .index;
        let trims = trim_event_graphs(&laid.bundle(), &parsed, &[tick]);
        assert!(trims.kept.is_empty(), "{:?}", trims.kept);
        let [trim] = trims.graphs.as_slice() else {
            panic!("one graph: {trims:?}");
        };
        let lines = dropped(trim);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains("Delay("), "{lines:?}");
        assert!(lines[2].contains("unless LocalVariable(Flag)"), "{lines:?}");
        assert_eq!(trim.parked, vec!["ReceiveTick".to_string()]);
        assert!(trim.text.contains("SwitchValue("), "the shared code stays");
        assert!(trim.text.contains("= 9"), "the dead tail stays");
        assert!(!trim.text.contains("Delay("));

        // The trimmed text saves, every stub still lands, and the graph is three statements
        // shorter.
        let edits = text_edit(trim.graph, &trim.text);
        let patched =
            patch_package_with(&laid.bundle(), Sidecars::default(), &parsed, &edits, None)
                .expect("save the trim");
        let after = parse_package(
            &AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            None,
        )
        .expect("reparse");
        verify_patch(&parsed, &after, &edits, &patched.applied).expect("verify");
        let was = parsed.exports[1].script.as_ref().unwrap().statements.len();
        let now = after.exports[1].script.as_ref().unwrap().statements.len();
        assert_eq!(was - now, 3);
    }

    #[test]
    fn removing_begin_play_takes_its_own_call_and_keeps_the_code_tick_falls_into() {
        let laid: Built = two_events_laid();
        let parsed = laid.parsed();
        let begin = parsed
            .exports
            .iter()
            .find(|e| e.object_name == "ReceiveBeginPlay")
            .expect("begin play")
            .index;
        let trims = trim_event_graphs(&laid.bundle(), &parsed, &[begin]);
        let [trim] = trims.graphs.as_slice() else {
            panic!("one graph: {trims:?}");
        };
        let lines = dropped(trim);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[1].contains("ReceiveBeginPlay()"), "{lines:?}");
        assert!(trim.text.contains("SwitchValue("));
        assert!(trim.text.contains("Delay("));
    }

    #[test]
    fn the_only_event_going_leaves_the_dispatch_and_its_label_on_the_return() {
        let built = event_graph().build();
        let parsed = built.parsed();
        let trims = trim_event_graphs(&built.bundle(), &parsed, &[2]);
        let [trim] = trims.graphs.as_slice() else {
            panic!("one graph: {trims:?}");
        };
        assert_eq!(
            trim.text,
            "Jump LocalVariable(EntryPoint)\n\
             @0092:\n\
             @000A:  ; ReceiveBeginPlay enters here\n\
             Return Nothing\n\
             EndOfScript\n"
        );
    }

    #[test]
    fn a_graph_that_has_to_keep_its_layout_keeps_its_code() {
        let built = event_graph().build();
        let mut parsed = built.parsed();
        parsed.exports[1]
            .script
            .as_mut()
            .unwrap()
            .resize_locks
            .push(crate::kismet::ResizeLock {
                kind: "test",
                reason: "something holds an offset into it".into(),
            });
        let trims = trim_event_graphs(&built.bundle(), &parsed, &[2]);
        assert!(trims.graphs.is_empty());
        assert!(
            trims.kept[0].contains("something holds an offset"),
            "{:?}",
            trims.kept
        );
    }
}
