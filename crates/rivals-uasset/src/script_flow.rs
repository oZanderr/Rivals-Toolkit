//! Reads a function's bytecode as control flow: the blocks its jumps and its flow stack divide it
//! into, the edges between them, and pseudo-code that nests what the jumps lay out as conditions,
//! loops and sequences.
//!
//! The pseudo-code keeps the statements in the order the compiler laid them out and prints each
//! exactly once. A jump it cannot nest stays a `goto` to a label, which is what keeps that true of
//! any layout, and the count of those is the measure of how readable a function came out.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;

use serde::Serialize;

use crate::kismet::{self, Expr, FixupKind, FixupSource, Script};
use crate::package::ParsedPackage;
use crate::script_text::ScriptText;

/// Deeper than any flow stack a compiled graph builds. A path past it is not followed further.
const MAX_STACK: usize = 64;
/// More flow states than any function needs to visit, so a pathological one cannot run away.
const MAX_STATES: usize = 200_000;
/// What a jump from outside the function counts as among a statement's predecessors.
const OUTSIDE: usize = usize::MAX;

#[derive(Debug, Clone, Serialize)]
pub struct ScriptFlow {
    pub blocks: Vec<FlowBlock>,
    pub edges: Vec<FlowEdge>,
    /// Where execution comes in: the function's start, an event graph's events and the points a
    /// latent action resumes at.
    pub entries: Vec<FlowEntry>,
    /// The pseudo-code, a line each.
    pub lines: Vec<FlowLine>,
    /// Jumps the pseudo-code could not nest, left as `goto`.
    pub gotos: usize,
    /// Flow stack paths not followed to their end, past the depth or state limits.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// A run of statements entered only at its first and left only after its last.
#[derive(Debug, Clone, Serialize)]
pub struct FlowBlock {
    /// The loaded offset of its first statement.
    pub start: u32,
    /// The loaded offset after its last statement.
    pub end: u32,
    /// Each statement's loaded offset.
    pub statements: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Into the next block, with nothing jumping.
    Fall,
    Jump,
    /// A conditional jump's way when its condition holds, which falls through.
    True,
    /// A conditional jump's way when its condition does not hold.
    False,
    /// Back to where a flow push said to go once the code after it was done.
    Pop,
    /// From an event graph's dispatch to one of its events.
    Dispatch,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct FlowEdge {
    pub from: usize,
    /// The block it leads to, or none where execution leaves the function.
    pub to: Option<usize>,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Start,
    Event,
    Resume,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowEntry {
    pub offset: u32,
    pub block: usize,
    pub kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowLine {
    pub depth: u32,
    pub text: String,
    /// The statements the line stands for, by loaded offset: one for a statement, none for a
    /// closing brace or a label.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub offsets: Vec<u32>,
}

impl ScriptFlow {
    /// The pseudo-code as text, four spaces a level.
    pub fn pseudo(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            for _ in 0..line.depth {
                out.push_str("    ");
            }
            out.push_str(&line.text);
            out.push('\n');
        }
        out
    }

    /// The graph in Graphviz's language, a box per block holding its first statement's label.
    pub fn dot(&self, name: &str) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "digraph \"{}\" {{", name.replace('"', "'"));
        let _ = writeln!(out, "  node [shape=box, fontname=monospace];");
        let _ = writeln!(out, "  exit [shape=doublecircle, label=\"exit\"];");
        for (at, block) in self.blocks.iter().enumerate() {
            let _ = writeln!(
                out,
                "  b{at} [label=\"@{:04X}..@{:04X}\\n{} statement(s)\"];",
                block.start,
                block.end,
                block.statements.len()
            );
        }
        for entry in &self.entries {
            let _ = writeln!(
                out,
                "  e{0:04X} [shape=plaintext, label=\"{1}\"];\n  e{0:04X} -> b{2};",
                entry.offset,
                entry
                    .note
                    .as_deref()
                    .unwrap_or(match entry.kind {
                        EntryKind::Start => "start",
                        EntryKind::Event => "event",
                        EntryKind::Resume => "resume",
                    })
                    .replace('"', "'"),
                entry.block
            );
        }
        for edge in &self.edges {
            let to = edge
                .to
                .map_or_else(|| "exit".to_string(), |to| format!("b{to}"));
            let style = match edge.kind {
                EdgeKind::Fall => "",
                EdgeKind::Jump => " [label=\"jump\"]",
                EdgeKind::True => " [label=\"true\"]",
                EdgeKind::False => " [label=\"false\", style=dashed]",
                EdgeKind::Pop => " [label=\"pop\", style=dotted]",
                EdgeKind::Dispatch => " [label=\"dispatch\", style=dotted]",
            };
            let _ = writeln!(out, "  b{} -> {to}{style};", edge.from);
        }
        out.push_str("}\n");
        out
    }
}

/// The control flow of export `export`'s script, printed as [`crate::print_script`] prints it.
/// `None` when the export holds no script, or one that did not decode whole.
pub fn script_flow(parsed: &ParsedPackage, export: u32) -> Option<ScriptFlow> {
    let text = crate::script_text::print_script(parsed, export)?;
    flow_with(parsed, export, &text)
}

/// [`script_flow`] over a text already printed, as a caller printing several functions of a
/// package with one [`crate::ScriptPrinter`] has it.
pub fn flow_with(parsed: &ParsedPackage, export: u32, text: &ScriptText) -> Option<ScriptFlow> {
    let script = parsed.exports.get(export as usize)?.script.as_ref()?;
    if !script.complete() || text.lines.len() != script.statements.len() {
        return None;
    }
    let own = parsed
        .exports
        .get(export as usize)
        .map(|e| e.object_name.as_str())
        .unwrap_or_default();
    let layout = Layout::of(script);
    let entries = entries(parsed, export, own, script, text, &layout);
    let (blocks, block_of) = blocks(script, &layout, &entries);
    let (edges, truncated) = edges(script, &layout, &block_of, &entries);
    let mut printer = Printer {
        script,
        text,
        layout: &layout,
        preds: predecessors(script, &layout, &entries),
        labels: BTreeSet::new(),
        lines: Vec::new(),
        gotos: 0,
    };
    printer.print(0, script.statements.len(), 0);
    let lines = printer.finish(&entries, text);
    let gotos = printer.gotos;
    Some(ScriptFlow {
        entries: entries
            .iter()
            .map(|entry| FlowEntry {
                offset: entry.offset,
                block: block_of.get(entry.at).copied().unwrap_or(0),
                kind: entry.kind,
                note: entry.note.clone(),
            })
            .collect(),
        blocks,
        edges,
        lines,
        gotos,
        truncated,
    })
}

/// Statement positions by loaded offset, with the end of the script as one past the last.
struct Layout {
    starts: Vec<u32>,
    end: u32,
}

impl Layout {
    fn of(script: &Script) -> Self {
        Self {
            starts: script.statements.iter().map(|s| s.offset).collect(),
            end: script.decoded_size,
        }
    }

    /// The statement starting at `offset`, the end of the script as the count of statements, or
    /// none for an offset inside a statement.
    fn index(&self, offset: u32) -> Option<usize> {
        if offset == self.end {
            return Some(self.starts.len());
        }
        self.starts.binary_search(&offset).ok()
    }
}

struct Entry {
    offset: u32,
    at: usize,
    kind: EntryKind,
    note: Option<String>,
}

/// The function's start, then each event and resume point, in the order they sit.
fn entries(
    parsed: &ParsedPackage,
    export: u32,
    own: &str,
    script: &Script,
    text: &ScriptText,
    layout: &Layout,
) -> Vec<Entry> {
    let mut found: BTreeMap<u32, EntryKind> = BTreeMap::new();
    if !script.statements.is_empty() {
        found.insert(0, EntryKind::Start);
    }
    let functions = kismet::functions_of(&parsed.exports);
    if let Some(function) = functions.iter().find(|f| f.export == export) {
        let inbound = kismet::inbound(&functions, function);
        for entry in &inbound.entries {
            found.entry(entry.offset).or_insert(EntryKind::Event);
        }
        for (_, fixup) in &inbound.linkages {
            if let FixupKind::Absolute { target, .. } = fixup.kind {
                found.entry(target).or_insert(EntryKind::Resume);
            }
        }
        for other in &functions {
            if other.event_graph.0 == export as i32 + 1 && other.event_graph.1 > 0 {
                found
                    .entry(other.event_graph.1 as u32)
                    .or_insert(EntryKind::Event);
            }
        }
    }
    for fixup in &script.fixups {
        if fixup.source == FixupSource::Linkage
            && let FixupKind::Absolute { target, resumes } = &fixup.kind
            && resumes.as_deref().is_none_or(|name| name == own)
        {
            found.entry(*target).or_insert(EntryKind::Resume);
        }
    }
    let notes: BTreeMap<u32, String> = text
        .lines
        .iter()
        .flat_map(|line| {
            line.labels
                .iter()
                .filter_map(move |label| Some((line.offset, label.note.clone()?)))
        })
        .collect();
    found
        .into_iter()
        .filter_map(|(offset, kind)| {
            let at = layout.index(offset)?;
            (at < script.statements.len()).then(|| Entry {
                offset,
                at,
                kind,
                note: notes.get(&offset).cloned(),
            })
        })
        .collect()
}

/// Where each statement goes besides the next one: its jump, its branch, or the code its push
/// sends a later pop to.
fn targets(expr: &Expr) -> Option<u32> {
    match expr {
        Expr::Jump { target }
        | Expr::JumpIfNot { target, .. }
        | Expr::PushExecutionFlow { target } => Some(*target),
        _ => None,
    }
}

/// Whether execution never reaches the statement after this one by falling into it.
fn ends(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Return { .. }
            | Expr::Jump { .. }
            | Expr::ComputedJump { .. }
            | Expr::Simple {
                name: "EndOfScript" | "PopExecutionFlow"
            }
    )
}

/// Whether a block ends after this statement: it goes somewhere other than the next one.
fn closes_block(expr: &Expr) -> bool {
    ends(expr)
        || matches!(
            expr,
            Expr::JumpIfNot { .. }
                | Expr::Unary {
                    name: "PopExecutionFlowIfNot",
                    ..
                }
        )
}

fn blocks(script: &Script, layout: &Layout, entries: &[Entry]) -> (Vec<FlowBlock>, Vec<usize>) {
    let count = script.statements.len();
    let mut leaders: BTreeSet<usize> = entries.iter().map(|entry| entry.at).collect();
    if count > 0 {
        leaders.insert(0);
    }
    for (at, statement) in script.statements.iter().enumerate() {
        if let Some(target) = targets(&statement.expr).and_then(|t| layout.index(t)) {
            leaders.insert(target);
        }
        if closes_block(&statement.expr) {
            leaders.insert(at + 1);
        }
    }
    leaders.retain(|&at| at < count);
    let starts: Vec<usize> = leaders.into_iter().collect();
    let mut block_of = vec![0; count];
    let mut blocks = Vec::with_capacity(starts.len());
    for (index, &start) in starts.iter().enumerate() {
        let stop = starts.get(index + 1).copied().unwrap_or(count);
        for slot in &mut block_of[start..stop] {
            *slot = index;
        }
        blocks.push(FlowBlock {
            start: layout.starts[start],
            end: layout.starts.get(stop).copied().unwrap_or(layout.end),
            statements: layout.starts[start..stop].to_vec(),
        });
    }
    (blocks, block_of)
}

/// The edges between blocks, following the flow stack along every path from every entry so a
/// pop leads where the push on that path said.
fn edges(
    script: &Script,
    layout: &Layout,
    block_of: &[usize],
    entries: &[Entry],
) -> (Vec<FlowEdge>, bool) {
    let count = script.statements.len();
    let mut found: BTreeSet<FlowEdge> = BTreeSet::new();
    let mut seen: HashSet<(usize, Vec<u32>)> = HashSet::new();
    // An event graph's events and resume points are reached through its dispatch, carrying what
    // its prologue pushed; anything else entering from outside starts with nothing pushed.
    let dispatched = script
        .statements
        .iter()
        .any(|s| matches!(s.expr, Expr::ComputedJump { .. }));
    let events: Vec<usize> = entries
        .iter()
        .filter(|entry| entry.kind != EntryKind::Start)
        .map(|entry| entry.at)
        .collect();
    let mut queue: Vec<(usize, Vec<u32>)> = entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::Start || !dispatched)
        .map(|entry| (entry.at, Vec::new()))
        .collect();
    let mut truncated = false;
    let block = |at: usize| (at < count).then(|| block_of[at]);
    while let Some((start, mut stack)) = queue.pop() {
        if seen.len() >= MAX_STATES {
            truncated = true;
            break;
        }
        if !seen.insert((start, stack.clone())) {
            continue;
        }
        let from = block_of[start];
        let mut at = start;
        let mut go = |to: Option<usize>, kind: EdgeKind, stack: Vec<u32>| {
            found.insert(FlowEdge {
                from,
                to: to.and_then(block),
                kind,
            });
            if let Some(to) = to.filter(|&to| to < count) {
                if stack.len() > MAX_STACK {
                    truncated = true;
                } else {
                    queue.push((to, stack));
                }
            }
        };
        loop {
            let expr = &script.statements[at].expr;
            if let Expr::PushExecutionFlow { target } = expr {
                stack.push(*target);
            }
            let next = at + 1;
            let last = closes_block(expr) || block(next) != Some(from);
            if !last {
                at = next;
                continue;
            }
            match expr {
                Expr::Jump { target } => go(layout.index(*target), EdgeKind::Jump, stack),
                Expr::JumpIfNot { target, .. } => {
                    go(Some(next), EdgeKind::True, stack.clone());
                    go(layout.index(*target), EdgeKind::False, stack);
                }
                Expr::Simple {
                    name: "PopExecutionFlow",
                } => {
                    let to = stack.pop().and_then(|t| layout.index(t));
                    go(to, EdgeKind::Pop, stack);
                }
                Expr::Unary {
                    name: "PopExecutionFlowIfNot",
                    ..
                } => {
                    go(Some(next), EdgeKind::True, stack.clone());
                    let to = stack.pop().and_then(|t| layout.index(t));
                    go(to, EdgeKind::Pop, stack);
                }
                Expr::ComputedJump { .. } => {
                    for &event in &events {
                        go(Some(event), EdgeKind::Dispatch, stack.clone());
                    }
                    if events.is_empty() {
                        go(None, EdgeKind::Dispatch, stack);
                    }
                }
                Expr::Return { .. }
                | Expr::Simple {
                    name: "EndOfScript",
                } => go(None, EdgeKind::Jump, stack),
                _ => go(Some(next), EdgeKind::Fall, stack),
            }
            break;
        }
    }
    (found.into_iter().collect(), truncated)
}

/// Each statement's predecessors other than the one before it: what jumps to it, the push whose
/// pop resumes at it, and [`OUTSIDE`] for an entry from outside the function.
fn predecessors(script: &Script, layout: &Layout, entries: &[Entry]) -> Vec<BTreeSet<usize>> {
    let mut preds = vec![BTreeSet::new(); script.statements.len() + 1];
    for (at, statement) in script.statements.iter().enumerate() {
        if let Some(target) = targets(&statement.expr).and_then(|t| layout.index(t)) {
            preds[target].insert(at);
        }
    }
    for entry in entries.iter().filter(|e| e.kind != EntryKind::Start) {
        preds[entry.at].insert(OUTSIDE);
    }
    preds
}

struct Printer<'a> {
    script: &'a Script,
    text: &'a ScriptText,
    layout: &'a Layout,
    preds: Vec<BTreeSet<usize>>,
    /// The statements a printed line names by label, which get one.
    labels: BTreeSet<usize>,
    lines: Vec<FlowLine>,
    gotos: usize,
}

impl Printer<'_> {
    fn line(&mut self, depth: u32, text: String, at: Option<usize>) {
        self.lines.push(FlowLine {
            depth,
            text,
            offsets: at
                .map(|at| vec![self.script.statements[at].offset])
                .unwrap_or_default(),
        });
    }

    fn label(&self, at: usize) -> String {
        let offset = self
            .layout
            .starts
            .get(at)
            .copied()
            .unwrap_or(self.layout.end);
        format!("@{offset:04X}")
    }

    /// The text of the expression statement `at` holds starting `skip` bytes after its token, or
    /// the whole statement's when the printer recorded none there.
    fn operand(&self, at: usize, skip: u32) -> String {
        let line = &self.text.lines[at];
        let start = line.offset + skip;
        line.ranges
            .iter()
            .find(|(offset, _)| *offset == start)
            .and_then(|(_, (from, to))| line.text.get(*from..*to))
            .unwrap_or(&line.text)
            .to_string()
    }

    /// Whether statements `from..to` are entered only from inside `after..to`: nothing outside
    /// jumps, pushes or enters into them, so they can print nested under what precedes them.
    fn closed(&self, from: usize, to: usize, after: usize) -> bool {
        (from..to).all(|at| {
            self.preds[at]
                .iter()
                .all(|&pred| pred != OUTSIDE && pred >= after && pred < to)
        })
    }

    fn back_jump(&self, at: usize, to: usize) -> bool {
        matches!(
            self.script.statements.get(at).map(|s| &s.expr),
            Some(Expr::Jump { target }) if self.layout.index(*target) == Some(to)
        )
    }

    /// Prints statements `lo..hi` at `depth`, nesting the conditions, loops and branches that sit
    /// wholly inside the range.
    fn print(&mut self, lo: usize, hi: usize, depth: u32) {
        let mut at = lo;
        while at < hi {
            let statement = &self.script.statements[at];
            match &statement.expr {
                Expr::JumpIfNot { target, .. } => {
                    let condition = self.operand(at, 5);
                    let Some(to) = self.layout.index(*target) else {
                        self.plain(at, depth);
                        at += 1;
                        continue;
                    };
                    // A loop: the condition first, a body jumping back to it, and the jump out
                    // landing just past that.
                    if to > at + 1
                        && to <= hi
                        && self.back_jump(to - 1, at)
                        && self.closed(at + 1, to - 1, at)
                    {
                        self.line(depth, format!("while ({condition}) {{"), Some(at));
                        self.print(at + 1, to - 1, depth + 1);
                        self.line(depth, "}".to_string(), Some(to - 1));
                        at = to;
                        continue;
                    }
                    if to > at && to <= hi && self.closed(at + 1, to, at) {
                        let join = match &self.script.statements[to - 1].expr {
                            Expr::Jump { target } if to > at + 1 => self
                                .layout
                                .index(*target)
                                .filter(|&join| join > to && join <= hi),
                            _ => None,
                        };
                        if let Some(join) = join.filter(|&join| self.closed(to, join, at)) {
                            self.line(depth, format!("if ({condition}) {{"), Some(at));
                            self.print(at + 1, to - 1, depth + 1);
                            self.line(depth, "} else {".to_string(), Some(to - 1));
                            self.print(to, join, depth + 1);
                            self.line(depth, "}".to_string(), None);
                            at = join;
                            continue;
                        }
                        self.line(depth, format!("if ({condition}) {{"), Some(at));
                        self.print(at + 1, to, depth + 1);
                        self.line(depth, "}".to_string(), None);
                        at = to;
                        continue;
                    }
                    self.labels.insert(to);
                    self.gotos += 1;
                    let label = self.label(to);
                    self.line(depth, format!("if (!({condition})) goto {label}"), Some(at));
                }
                Expr::Jump { target } => match self.layout.index(*target) {
                    Some(to) => {
                        self.labels.insert(to);
                        self.gotos += 1;
                        let label = self.label(to);
                        self.line(depth, format!("goto {label}"), Some(at));
                    }
                    None => self.plain(at, depth),
                },
                Expr::PushExecutionFlow { target } => match self.layout.index(*target) {
                    Some(to) => {
                        self.labels.insert(to);
                        let label = self.label(to);
                        self.line(
                            depth,
                            format!("push {label}  // runs once the code below pops"),
                            Some(at),
                        );
                    }
                    None => self.plain(at, depth),
                },
                Expr::Simple {
                    name: "PopExecutionFlow",
                } => self.line(depth, "pop".to_string(), Some(at)),
                Expr::Unary {
                    name: "PopExecutionFlowIfNot",
                    ..
                } => {
                    let condition = self.operand(at, 1);
                    self.line(depth, format!("if (!({condition})) pop"), Some(at));
                }
                Expr::ComputedJump { .. } => {
                    let entry = self.operand(at, 1);
                    self.line(depth, format!("dispatch on {entry}"), Some(at));
                }
                Expr::Return { value } => {
                    let text = match value.as_ref() {
                        Expr::Simple { name: "Nothing" } => "return".to_string(),
                        _ => format!("return {}", self.operand(at, 1)),
                    };
                    self.line(depth, text, Some(at));
                }
                Expr::Simple {
                    name: "EndOfScript",
                } => self.line(depth, "end".to_string(), Some(at)),
                _ => self.plain(at, depth),
            }
            at += 1;
        }
    }

    fn plain(&mut self, at: usize, depth: u32) {
        let line = &self.text.lines[at];
        let text = match &line.note {
            Some(note) => format!("{}  // {note}", line.text),
            None => line.text.clone(),
        };
        self.line(depth, text, Some(at));
    }

    /// The lines with a label before each statement something names by one, and before each
    /// entry, saying what enters there.
    fn finish(&mut self, entries: &[Entry], text: &ScriptText) -> Vec<FlowLine> {
        let mut wanted: BTreeMap<usize, Option<String>> =
            self.labels.iter().map(|&at| (at, None)).collect();
        for entry in entries.iter().filter(|e| e.kind != EntryKind::Start) {
            wanted.insert(entry.at, entry.note.clone());
        }
        let count = self.script.statements.len();
        let mut out = Vec::with_capacity(self.lines.len() + wanted.len());
        let first_line: BTreeMap<u32, usize> = self
            .lines
            .iter()
            .enumerate()
            .rev()
            .flat_map(|(index, line)| line.offsets.iter().map(move |&offset| (offset, index)))
            .collect();
        let mut before: BTreeMap<usize, Vec<FlowLine>> = BTreeMap::new();
        for (at, note) in wanted {
            let label = self.label(at);
            let text = match note {
                Some(note) => format!("{label}:  // {note}"),
                None => format!("{label}:"),
            };
            let place = if at < count {
                first_line.get(&self.script.statements[at].offset).copied()
            } else {
                None
            };
            let depth = place.map_or(0, |index| self.lines[index].depth);
            before
                .entry(place.unwrap_or(usize::MAX))
                .or_default()
                .push(FlowLine {
                    depth,
                    text,
                    offsets: Vec::new(),
                });
        }
        for (index, line) in std::mem::take(&mut self.lines).into_iter().enumerate() {
            if let Some(labels) = before.remove(&index) {
                out.extend(labels);
            }
            out.push(line);
        }
        if let Some(end) = before.remove(&usize::MAX) {
            out.extend(end);
        }
        if let Some(stopped) = &text.stopped {
            out.push(FlowLine {
                depth: 0,
                text: format!("// {stopped}"),
                offsets: Vec::new(),
            });
        }
        out
    }
}

/// Why a function's pseudo-code does not print each statement exactly once, when it does not.
pub fn flow_fault(script: &Script, flow: &ScriptFlow) -> Option<String> {
    let mut printed: BTreeMap<u32, usize> = BTreeMap::new();
    for line in &flow.lines {
        for &offset in &line.offsets {
            *printed.entry(offset).or_default() += 1;
        }
    }
    for statement in &script.statements {
        match printed.get(&statement.offset).copied().unwrap_or(0) {
            1 => {}
            0 => {
                return Some(format!(
                    "statement @{:04X} is not printed",
                    statement.offset
                ));
            }
            n => {
                return Some(format!(
                    "statement @{:04X} is printed {n} times",
                    statement.offset
                ));
            }
        }
    }
    (printed.len() != script.statements.len())
        .then(|| "a line stands for an offset no statement starts at".to_string())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The two-event graph laid out as an if and an else, a sequence and a while loop, then a
    /// second event's code.
    const SHAPES: &str = "Jump LocalVariable(EntryPoint)\n\
        @000A:\n\
        Jump @else unless LocalVariable(Flag)\n\
        Let LocalVariable(Count) = 1\n\
        Jump @join\n\
        @else:\n\
        Let LocalVariable(Count) = 2\n\
        @join:\n\
        PushExecutionFlow @loop\n\
        Let LocalVariable(Count) = 3\n\
        PopExecutionFlow\n\
        @loop:\n\
        Jump @done unless LocalVariable(Flag)\n\
        Let LocalVariable(Count) = 4\n\
        Jump @loop\n\
        @done:\n\
        Return Nothing\n\
        @0047:\n\
        Return Nothing\n\
        EndOfScript\n";

    fn laid(text: &str) -> ParsedPackage {
        let built = crate::script_fixture::event_graph_two_events().build();
        let before = built.parsed();
        let edits = crate::edit::PackageEdits {
            script_texts: vec![crate::edit::ScriptTextEdit {
                export: 1,
                text: text.to_string(),
                was: None,
                object: None,
            }],
            ..Default::default()
        };
        let patched = crate::edit::patch_package_with(
            &built.bundle(),
            crate::edit::Sidecars::default(),
            &before,
            &edits,
            None,
        )
        .expect("the graph lays out");
        crate::parse_package(
            &crate::AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            None,
        )
        .expect("it parses")
    }

    #[test]
    fn conditions_loops_and_sequences_nest_and_print_each_statement_once() {
        let parsed = laid(SHAPES);
        let flow = script_flow(&parsed, 1).expect("a flow");
        let script = parsed.exports[1].script.as_ref().unwrap();
        assert_eq!(flow_fault(script, &flow), None);
        assert_eq!(flow.gotos, 0, "{}", flow.pseudo());
        let pseudo = flow.pseudo();
        let shape: Vec<&str> = pseudo
            .lines()
            .map(|line| line.split_whitespace().next().unwrap_or(""))
            .collect();
        assert!(
            pseudo.contains("if (LocalVariable(Flag)) {\n    Let"),
            "{pseudo}"
        );
        assert!(pseudo.contains("} else {\n    Let"), "{pseudo}");
        assert!(
            pseudo.contains("while (LocalVariable(Flag)) {\n    Let"),
            "{pseudo}"
        );
        assert!(shape.contains(&"push"), "{pseudo}");
        assert!(shape.contains(&"pop"), "{pseudo}");
        assert!(
            pseudo.starts_with("dispatch on LocalVariable(EntryPoint)"),
            "{pseudo}"
        );
        assert_eq!(
            flow.entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::Event)
                .count(),
            2
        );
    }

    #[test]
    fn a_pop_leads_where_its_push_said() {
        let parsed = laid(SHAPES);
        let flow = script_flow(&parsed, 1).expect("a flow");
        let pop = flow
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Pop)
            .expect("a pop edge");
        let to = &flow.blocks[pop.to.expect("into the function")];
        let push = parsed.exports[1]
            .script
            .as_ref()
            .unwrap()
            .statements
            .iter()
            .find_map(|s| match s.expr {
                Expr::PushExecutionFlow { target } => Some(target),
                _ => None,
            })
            .unwrap();
        assert_eq!(to.start, push);
        assert!(flow.dot("graph").contains("[label=\"pop\", style=dotted]"));
    }

    /// A jump into the middle of a branch from outside it cannot nest, so it stays a goto and the
    /// statements still print once each.
    #[test]
    fn a_jump_into_a_branch_stays_a_goto() {
        let parsed = laid(
            "Jump LocalVariable(EntryPoint)\n\
             @000A:\n\
             Jump @skip unless LocalVariable(Flag)\n\
             Let LocalVariable(Count) = 1\n\
             @inside:\n\
             Let LocalVariable(Count) = 2\n\
             @skip:\n\
             Jump @inside unless LocalVariable(Flag)\n\
             @0047:\n\
             Return Nothing\n\
             EndOfScript\n",
        );
        let flow = script_flow(&parsed, 1).expect("a flow");
        let script = parsed.exports[1].script.as_ref().unwrap();
        assert_eq!(flow_fault(script, &flow), None);
        assert!(flow.gotos >= 1, "{}", flow.pseudo());
        assert!(flow.pseudo().contains("goto @"), "{}", flow.pseudo());
    }
}
