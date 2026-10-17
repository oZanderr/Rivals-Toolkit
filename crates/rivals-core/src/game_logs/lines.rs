//! Indexes a decoded log line by line, reading each line's verbosity and category from the prefix the engine writes.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::decode::DecodedLog;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verbosity {
    Fatal,
    Error,
    Warning,
    Display,
    Log,
    Verbose,
}

impl Verbosity {
    pub const ALL: [Verbosity; 6] = [
        Self::Fatal,
        Self::Error,
        Self::Warning,
        Self::Display,
        Self::Log,
        Self::Verbose,
    ];

    fn from_word(word: &str) -> Option<Self> {
        Some(match word {
            "Fatal" => Self::Fatal,
            "Error" => Self::Error,
            "Warning" => Self::Warning,
            "Display" => Self::Display,
            "Verbose" | "VeryVerbose" => Self::Verbose,
            _ => return None,
        })
    }

    fn from_u8(value: u8) -> Self {
        Self::ALL
            .get(usize::from(value))
            .copied()
            .unwrap_or(Self::Log)
    }

    pub fn is_problem(self) -> bool {
        matches!(self, Self::Fatal | Self::Error | Self::Warning)
    }
}

/// What a line's prefix says about it. A line with no prefix at all continues the one before,
/// as a Python traceback or the rest of a fatal error does.
#[derive(Debug, PartialEq, Eq)]
pub struct Prefix<'a> {
    pub category: Option<&'a str>,
    pub verbosity: Option<Verbosity>,
    pub message: &'a str,
    pub prefixed: bool,
}

/// `Word: ` at the start of `text`, and what follows it.
fn take_word(text: &str) -> Option<(&str, &str)> {
    let len = text
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    if len == 0 {
        return None;
    }
    let rest = text[len..].strip_prefix(':')?;
    if rest.is_empty() {
        return Some((&text[..len], rest));
    }
    Some((&text[..len], rest.strip_prefix(' ')?))
}

/// `[2026.10.14-02.14.00:151 -0700][  42]`, the timestamp and frame counter some builds write.
fn strip_timestamp(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let stamped = bytes.first() == Some(&b'[')
        && bytes
            .get(1..5)
            .is_some_and(|y| y.iter().all(u8::is_ascii_digit))
        && bytes.get(5) == Some(&b'.');
    if !stamped {
        return None;
    }
    let after = &line[line.find(']')? + 1..];
    let frame = after.strip_prefix('[')?;
    let close = frame.find(']')?;
    frame[..close]
        .trim()
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| &frame[close + 1..])
}

pub fn split_prefix(line: &str) -> Prefix<'_> {
    let mut rest = line;
    let mut prefixed = false;
    if let Some(after) = strip_timestamp(rest) {
        rest = after;
        prefixed = true;
    }
    if let Some(after) = rest.strip_prefix("[Bid:")
        && let Some(close) = after.find(']')
    {
        rest = &after[close + 1..];
        prefixed = true;
    }

    let (category, verbosity, message) = match take_word(rest) {
        Some((word, after)) => match Verbosity::from_word(word) {
            Some(verbosity) => (None, Some(verbosity), after),
            None => match take_word(after)
                .and_then(|(next, after)| Some((Verbosity::from_word(next)?, after)))
            {
                Some((verbosity, after)) => (Some(word), Some(verbosity), after),
                // `LogTemp: text` is Log verbosity; any other `Word:` is text, not a category.
                None if word.starts_with("Log") => (Some(word), Some(Verbosity::Log), after),
                None => (None, None, rest),
            },
        },
        None => (None, None, rest),
    };
    Prefix {
        category,
        verbosity,
        message,
        prefixed: prefixed || verbosity.is_some(),
    }
}

const CONTINUATION: u8 = 1;
const TRAILER_DUP: u8 = 2;

/// A decoded log with every line's place and kind. Offsets are `u32` to keep a 600k-line log's
/// index small; a log past 4 GB is refused.
pub struct ParsedLog {
    text: String,
    starts: Vec<u32>,
    ends: Vec<u32>,
    verbosity: Vec<u8>,
    category: Vec<u16>,
    flags: Vec<u8>,
    /// Index 0 is "no category".
    categories: Vec<String>,
    trailer_from: Option<usize>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogFilter {
    /// Empty keeps every verbosity.
    #[serde(default)]
    pub verbosities: Vec<Verbosity>,
    /// Empty keeps every category; `""` stands for lines with none.
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryCount {
    pub name: String,
    pub count: u32,
}

impl ParsedLog {
    pub fn new(decoded: DecodedLog) -> Result<Self, String> {
        if u32::try_from(decoded.bytes.len()).is_err() {
            return Err("this log is over 4 GB decoded".to_string());
        }
        // Counted in the raw bytes, before any lossy repair moves offsets.
        let trailer_line = decoded
            .trailer_offset
            .map(|at| decoded.bytes[..at].iter().filter(|&&b| b == b'\n').count());
        let text = String::from_utf8(decoded.bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());

        let mut starts = Vec::new();
        let mut ends = Vec::new();
        let mut at = 0usize;
        for piece in text.split_inclusive('\n') {
            let line = piece.trim_end_matches('\n').trim_end_matches('\r');
            starts.push(at as u32);
            ends.push((at + line.len()) as u32);
            at += piece.len();
        }

        let mut verbosity = Vec::with_capacity(starts.len());
        let mut category = Vec::with_capacity(starts.len());
        let mut flags = Vec::with_capacity(starts.len());
        let mut categories = vec![String::new()];
        let mut category_ids: HashMap<&str, u16> = HashMap::new();
        let (mut last_verbosity, mut last_category) = (Verbosity::Log, 0u16);
        for (&start, &end) in starts.iter().zip(&ends) {
            let prefix = split_prefix(&text[start as usize..end as usize]);
            if prefix.prefixed {
                last_verbosity = prefix.verbosity.unwrap_or(Verbosity::Log);
                last_category = match prefix.category {
                    None => 0,
                    Some(name) => match category_ids.get(name) {
                        Some(&id) => id,
                        None => {
                            let id = u16::try_from(categories.len()).unwrap_or(u16::MAX);
                            if id < u16::MAX {
                                categories.push(name.to_string());
                                category_ids.insert(name, id);
                            }
                            id
                        }
                    },
                };
                flags.push(0);
            } else {
                flags.push(CONTINUATION);
            }
            verbosity.push(last_verbosity as u8);
            category.push(last_category);
        }
        drop(category_ids);

        let mut parsed = Self {
            text,
            starts,
            ends,
            verbosity,
            category,
            flags,
            categories,
            trailer_from: trailer_line,
            notes: decoded.notes,
        };
        parsed.mark_trailer_repeats();
        Ok(parsed)
    }

    /// The trailer is the tail of the log flushed a second time, so lines the records already
    /// hold are flagged and left out of the analysis.
    fn mark_trailer_repeats(&mut self) {
        let Some(from) = self.trailer_from.filter(|&from| from < self.len()) else {
            return;
        };
        let separator = from.saturating_sub(1);
        let window = (self.len() - from) * 2 + 64;
        let recent: HashSet<&str> = (separator.saturating_sub(window)..separator)
            .map(|n| self.line(n))
            .collect();
        let first = self.line(from);
        let mut dup: Vec<usize> = (from..self.len())
            .filter(|&n| recent.contains(self.line(n)))
            .collect();
        // The ring dump starts mid-line.
        if !first.is_empty() && !dup.contains(&from) && recent.iter().any(|l| l.ends_with(first)) {
            dup.push(from);
        }
        for n in dup {
            self.flags[n] |= TRAILER_DUP;
        }
    }

    pub fn len(&self) -> usize {
        self.starts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn line(&self, n: usize) -> &str {
        match (self.starts.get(n), self.ends.get(n)) {
            (Some(&start), Some(&end)) => &self.text[start as usize..end as usize],
            _ => "",
        }
    }

    pub fn verbosity(&self, n: usize) -> Verbosity {
        Verbosity::from_u8(self.verbosity.get(n).copied().unwrap_or(4))
    }

    pub fn category(&self, n: usize) -> &str {
        self.category
            .get(n)
            .and_then(|&id| self.categories.get(usize::from(id)))
            .map_or("", String::as_str)
    }

    pub fn is_continuation(&self, n: usize) -> bool {
        self.flags.get(n).is_some_and(|f| f & CONTINUATION != 0)
    }

    pub fn is_trailer(&self, n: usize) -> bool {
        self.trailer_from.is_some_and(|from| n >= from)
    }

    pub fn is_trailer_repeat(&self, n: usize) -> bool {
        self.flags.get(n).is_some_and(|f| f & TRAILER_DUP != 0)
    }

    /// The line `offset` falls in.
    fn line_of(&self, offset: usize) -> usize {
        self.starts
            .partition_point(|&start| start as usize <= offset)
            .saturating_sub(1)
    }

    pub fn verbosity_counts(&self) -> Vec<(Verbosity, u32)> {
        let mut counts = [0u32; 6];
        for &v in &self.verbosity {
            if let Some(count) = counts.get_mut(usize::from(v)) {
                *count += 1;
            }
        }
        Verbosity::ALL.into_iter().zip(counts).collect()
    }

    /// Categories by how many lines carry them, the most first.
    pub fn category_counts(&self) -> Vec<CategoryCount> {
        let mut counts = vec![0u32; self.categories.len()];
        for &c in &self.category {
            if let Some(count) = counts.get_mut(usize::from(c)) {
                *count += 1;
            }
        }
        let mut out: Vec<CategoryCount> = self
            .categories
            .iter()
            .zip(counts)
            .filter(|(_, count)| *count > 0)
            .map(|(name, count)| CategoryCount {
                name: name.clone(),
                count,
            })
            .collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
        out
    }

    /// The lines `filter` keeps, in order.
    pub fn filter(&self, filter: &LogFilter) -> Result<Vec<u32>, String> {
        let verbosity_ok: [bool; 6] = std::array::from_fn(|i| {
            filter.verbosities.is_empty() || filter.verbosities.contains(&Verbosity::ALL[i])
        });
        let category_ok: Vec<bool> = self
            .categories
            .iter()
            .map(|name| filter.categories.is_empty() || filter.categories.contains(name))
            .collect();
        let keeps = |n: usize| {
            verbosity_ok
                .get(usize::from(self.verbosity[n]))
                .copied()
                .unwrap_or(true)
                && category_ok
                    .get(usize::from(self.category[n]))
                    .copied()
                    .unwrap_or(true)
        };

        if filter.text.is_empty() {
            return Ok((0..self.len())
                .filter(|&n| keeps(n))
                .map(|n| n as u32)
                .collect());
        }
        let pattern = regex::RegexBuilder::new(&regex::escape(&filter.text))
            .case_insensitive(!filter.case_sensitive)
            .build()
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut last = None;
        for found in pattern.find_iter(&self.text) {
            let n = self.line_of(found.start());
            // A match in the CR or newline after a line's text is not in the line.
            if last == Some(n) || found.end() > self.ends[n] as usize || !keeps(n) {
                continue;
            }
            last = Some(n);
            out.push(n as u32);
        }
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::super::decode::{DecodedLog, TRAILER_SEPARATOR};
    use super::*;

    fn parsed(text: &str, trailer: Option<&str>) -> ParsedLog {
        let mut bytes = text.as_bytes().to_vec();
        let mut trailer_offset = None;
        if let Some(trailer) = trailer {
            bytes.extend_from_slice(TRAILER_SEPARATOR.as_bytes());
            trailer_offset = Some(bytes.len());
            bytes.extend_from_slice(trailer.as_bytes());
        }
        ParsedLog::new(DecodedLog {
            bytes,
            key_source: None,
            records: 1,
            trailer_offset,
            notes: Vec::new(),
        })
        .unwrap()
    }

    #[test]
    fn reads_every_prefix_shape_the_game_writes() {
        let p = split_prefix("[2026.10.14-02.14.00:151 -0700][     0]LogConfig: Display: Set CVar");
        assert_eq!(
            (p.category, p.verbosity, p.message),
            (Some("LogConfig"), Some(Verbosity::Display), "Set CVar")
        );
        let p = split_prefix("[Bid:6697103_1791969369_13]Error: bind event failed");
        assert_eq!(
            (p.category, p.verbosity, p.message),
            (None, Some(Verbosity::Error), "bind event failed")
        );
        let p = split_prefix("LogPakFile: Warning: IoStore container not found");
        assert_eq!(p.category, Some("LogPakFile"));
        assert_eq!(p.verbosity, Some(Verbosity::Warning));
        let p = split_prefix("LogTemp: plain log line");
        assert_eq!(
            (p.category, p.verbosity),
            (Some("LogTemp"), Some(Verbosity::Log))
        );
        let p = split_prefix("RecordHistogramMetric: Display: x");
        assert_eq!(p.category, Some("RecordHistogramMetric"));
        let p = split_prefix("  File \"lib/threading.py\", line 1045 in _bootstrap_inner");
        assert!(!p.prefixed);
        let p = split_prefix("GameThread timed out waiting for RenderThread after 60.00 secs");
        assert!(!p.prefixed);
        let p = split_prefix("Error:");
        assert_eq!((p.verbosity, p.message), (Some(Verbosity::Error), ""));
    }

    #[test]
    fn unprefixed_lines_continue_the_line_before() {
        let log = parsed(
            "LogInit: Display: start\r\nLogWindows: Error: appError called: Fatal error\r\nGameThread timed out\r\n  File \"x.py\"\r\n",
            None,
        );
        assert_eq!(log.len(), 4);
        assert_eq!(
            log.line(1),
            "LogWindows: Error: appError called: Fatal error"
        );
        assert_eq!(log.verbosity(2), Verbosity::Error);
        assert_eq!(log.category(3), "LogWindows");
        assert!(log.is_continuation(3));
        assert!(!log.is_continuation(1));
    }

    #[test]
    fn trailer_lines_the_records_hold_are_flagged() {
        let log = parsed(
            "LogA: Display: one\nLogA: Display: two\nLogA: Display: three\n",
            Some("gA: Display: two\nLogA: Display: three\nLogA: Display: four\n"),
        );
        let trailer: Vec<usize> = (0..log.len()).filter(|&n| log.is_trailer(n)).collect();
        assert_eq!(trailer, [4, 5, 6]);
        assert!(log.is_trailer_repeat(4));
        assert!(log.is_trailer_repeat(5));
        assert!(!log.is_trailer_repeat(6));
    }

    #[test]
    fn filters_by_verbosity_category_and_text() {
        let log = parsed(
            "LogA: Display: alpha\nLogB: Warning: Beta\nLogA: Error: gamma beta\ncontinued beta\n",
            None,
        );
        let warnings_up = LogFilter {
            verbosities: vec![Verbosity::Warning, Verbosity::Error],
            ..LogFilter::default()
        };
        assert_eq!(log.filter(&warnings_up).unwrap(), [1, 2, 3]);
        let beta = LogFilter {
            text: "beta".to_string(),
            ..LogFilter::default()
        };
        assert_eq!(log.filter(&beta).unwrap(), [1, 2, 3]);
        let exact = LogFilter {
            text: "beta".to_string(),
            case_sensitive: true,
            categories: vec!["LogA".to_string()],
            ..LogFilter::default()
        };
        assert_eq!(log.filter(&exact).unwrap(), [2, 3]);
    }

    #[test]
    fn counts_categories_and_verbosities() {
        let log = parsed("LogA: Error: x\nLogA: Display: y\nLogB: Display: z\n", None);
        let categories = log.category_counts();
        assert_eq!(categories[0].name, "LogA");
        assert_eq!(categories[0].count, 2);
        let counts: HashMap<Verbosity, u32> = log.verbosity_counts().into_iter().collect();
        assert_eq!(counts[&Verbosity::Display], 2);
        assert_eq!(counts[&Verbosity::Error], 1);
    }
}
