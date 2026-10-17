//! Splits an engine log file into its framed encrypted records and the plaintext trailer after them.

pub const MAGIC: &[u8] = b"logversion000";
const SHM_BEGIN: &[u8] = b"<SHM_LOG_BEGIN>";
const SHM_END: &[u8] = b"<SHM_LOG_END>";

pub struct Record<'a> {
    pub offset: usize,
    pub payload: &'a [u8],
}

/// A parsed log file. A log from a process that died mid-write is the normal case, so a malformed
/// tail stops the walk with a `note` instead of failing, keeping the records already read.
#[derive(Default)]
pub struct Container<'a> {
    pub records: Vec<Record<'a>>,
    /// The `SharedMemoryLog` ring the device flushes verbatim after the last record: not framed,
    /// not encrypted, and starting mid-line.
    pub trailer: Option<&'a [u8]>,
    pub note: Option<String>,
}

impl Container<'_> {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty() && self.trailer.is_none()
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize, to: usize) -> Option<usize> {
    let to = to.min(haystack.len());
    if from >= to || to - from < needle.len() {
        return None;
    }
    haystack[from..to]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at)
}

fn read_i32(blob: &[u8], at: usize) -> Option<i32> {
    let bytes: [u8; 4] = blob.get(at..at + 4)?.try_into().ok()?;
    Some(i32::from_le_bytes(bytes))
}

fn take_trailer<'a>(blob: &'a [u8], container: &mut Container<'a>, at: usize) {
    let mut start = at + SHM_BEGIN.len();
    if blob.get(start) == Some(&b'\n') {
        start += 1;
    }
    let end = find(blob, SHM_END, start, blob.len());
    if end.is_none() {
        container.note = Some("the shared-memory trailer has no <SHM_LOG_END>".to_string());
    }
    container.trailer = blob.get(start..end.unwrap_or(blob.len()));
}

pub fn parse(blob: &[u8]) -> Result<Container<'_>, String> {
    if !blob.starts_with(MAGIC) {
        return Err("not a Marvel Rivals engine log (no logversion000 header)".to_string());
    }
    let mut container = Container::default();
    let mut offset = MAGIC.len();
    while offset < blob.len() {
        // The trailer follows the last record's size echo after one newline.
        if let Some(at) = find(blob, SHM_BEGIN, offset, offset + 8 + SHM_BEGIN.len()) {
            take_trailer(blob, &mut container, at);
            return Ok(container);
        }
        let Some(size) = read_i32(blob, offset) else {
            container.note = Some(format!(
                "truncated size prefix at 0x{offset:x} ({} byte(s) left)",
                blob.len() - offset
            ));
            return Ok(container);
        };
        let fits = usize::try_from(size)
            .ok()
            .filter(|&size| offset + 8 + size <= blob.len());
        let Some(size) = fits else {
            match find(blob, SHM_BEGIN, offset, blob.len()) {
                Some(at) => {
                    if at - offset > 1 {
                        container.note = Some(format!(
                            "{} unframed byte(s) before the shared-memory trailer at 0x{at:x}",
                            at - offset
                        ));
                    }
                    take_trailer(blob, &mut container, at);
                }
                None => {
                    container.note = Some(format!(
                        "record {}: implausible size {size} at 0x{offset:x}",
                        container.records.len()
                    ));
                }
            }
            return Ok(container);
        };
        let echo = read_i32(blob, offset + 4 + size);
        if echo != i32::try_from(size).ok() {
            container.note = Some(format!(
                "record {}: size suffix {} does not match prefix {size} at 0x{offset:x}",
                container.records.len(),
                echo.unwrap_or(-1)
            ));
            return Ok(container);
        }
        container.records.push(Record {
            offset,
            payload: &blob[offset + 4..offset + 4 + size],
        });
        offset += 8 + size;
    }
    Ok(container)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn frame(payloads: &[&[u8]], trailer: Option<&[u8]>) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        for payload in payloads {
            let size = (payload.len() as i32).to_le_bytes();
            out.extend_from_slice(&size);
            out.extend_from_slice(payload);
            out.extend_from_slice(&size);
        }
        if let Some(trailer) = trailer {
            out.extend_from_slice(b"\n<SHM_LOG_BEGIN>\n");
            out.extend_from_slice(trailer);
            out.extend_from_slice(b"<SHM_LOG_END>");
        }
        out
    }

    #[test]
    fn walks_every_record_and_the_trailer() {
        let blob = frame(&[b"one", b"two!"], Some(b"tail text\n"));
        let container = parse(&blob).unwrap();
        let payloads: Vec<&[u8]> = container.records.iter().map(|r| r.payload).collect();
        assert_eq!(payloads, [&b"one"[..], b"two!"]);
        assert_eq!(container.trailer, Some(&b"tail text\n"[..]));
        assert!(container.note.is_none());
    }

    #[test]
    fn a_truncated_tail_keeps_the_records_before_it() {
        let mut blob = frame(&[b"one", b"two"], None);
        blob.truncate(blob.len() - 2);
        let container = parse(&blob).unwrap();
        assert_eq!(container.records.len(), 1);
        assert!(container.note.unwrap().contains("implausible size"));
    }

    #[test]
    fn a_mismatched_echo_stops_the_walk() {
        let mut blob = frame(&[b"one", b"two"], None);
        let last = blob.len() - 1;
        blob[last] = 9;
        let container = parse(&blob).unwrap();
        assert_eq!(container.records.len(), 1);
        assert!(container.note.unwrap().contains("does not match"));
    }

    #[test]
    fn a_trailer_only_log_has_no_records() {
        let blob = frame(&[], Some(b"ring dump"));
        let container = parse(&blob).unwrap();
        assert!(container.records.is_empty());
        assert_eq!(container.trailer, Some(&b"ring dump"[..]));
    }

    #[test]
    fn an_unterminated_trailer_runs_to_the_end() {
        let mut blob = frame(&[b"one"], None);
        blob.extend_from_slice(b"\n<SHM_LOG_BEGIN>\nhalf");
        let container = parse(&blob).unwrap();
        assert_eq!(container.trailer, Some(&b"half"[..]));
        assert!(container.note.unwrap().contains("<SHM_LOG_END>"));
    }

    #[test]
    fn the_bare_header_is_empty_and_anything_else_is_refused() {
        assert!(parse(MAGIC).unwrap().is_empty());
        assert!(parse(b"not a log").is_err());
    }
}
