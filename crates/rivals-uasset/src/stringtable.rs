//! Reads the entries a UStringTable writes after its own properties.

use serde::Serialize;

use crate::props::{Ctx, Diagnostics};
use crate::reader::Cursor;
use crate::stock::{StockGap, StockGapKind};

#[derive(Debug, Clone, Serialize)]
pub struct StringTable {
    pub namespace: String,
    pub entries: Vec<StringTableEntry>,
    /// Metadata the table keys on strings no entry carries.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub loose_metadata: Vec<StringTableMetaData>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StringTableMetaData {
    pub key: String,
    pub pairs: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StringTableEntry {
    pub key: String,
    pub source: String,
    /// A marker string this game writes after each source: `Encrypt` on some lobby entries, empty
    /// everywhere else.
    pub tag: String,
    /// Name and value pairs from the metadata map that closes the table. Cooked tables rarely
    /// carry any.
    pub metadata: Vec<(String, String)>,
}

/// Where a table's bytes sit, for edits that change a string or the number of entries.
#[derive(Debug, Clone)]
pub struct StringTableLayout {
    pub export: u32,
    pub namespace: (u64, u64),
    /// The `i32` holding how many entries follow.
    pub count_at: u64,
    pub entries: Vec<StringEntrySpan>,
    /// The count of the metadata records that follow the entries, which an added entry has to stay
    /// in front of.
    pub trailer_at: u64,
    /// The metadata records in stream order.
    pub records: Vec<MetaRecordSpan>,
    /// Where the table ends, which is where a new record goes.
    pub end: u64,
}

/// One record of the metadata map: the entry key it is about, then its items.
#[derive(Debug, Clone)]
pub struct MetaRecordSpan {
    pub key: String,
    pub start: u64,
    /// The `i32` holding how many items follow.
    pub count_at: u64,
    pub items: Vec<MetaItemSpan>,
    pub end: u64,
}

/// One metadata item: an `FName` id, then an `FString` value.
#[derive(Debug, Clone)]
pub struct MetaItemSpan {
    pub id: String,
    pub start: u64,
    pub value: (u64, u64),
}

#[derive(Debug, Clone)]
pub struct StringEntrySpan {
    pub key: (u64, u64),
    pub source: (u64, u64),
    /// Where the marker string after the source starts; an empty one is a zero word.
    pub tag_at: u64,
    pub end: u64,
}

/// Reads a table as this game lays it out, or failing that as a stock editor does, with no marker
/// after each source. A stock table is read for what it holds and its gaps recorded, but keeps no
/// layout, so its entries are not edited until a conversion has added the markers.
pub(crate) fn read_either_layout(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    export: u32,
    diagnostics: &mut Diagnostics,
) -> Result<StringTable, String> {
    let (at, names, marks, layouts) = (
        cursor.position(),
        cursor.names_len(),
        diagnostics.marks(),
        diagnostics.string_tables.len(),
    );
    let game = read_string_table(cursor, ctx, export, diagnostics, true);
    if game.is_ok() && cursor.remaining() == 0 {
        return game;
    }
    let rewind = |cursor: &mut Cursor<'_>, diagnostics: &mut Diagnostics| {
        cursor.truncate_names(names);
        diagnostics.rewind(&marks);
        diagnostics.string_tables.truncate(layouts);
        cursor.seek_to(at)
    };
    rewind(cursor, diagnostics)?;
    match read_string_table(cursor, ctx, export, diagnostics, false) {
        Ok(table) if cursor.remaining() == 0 => Ok(table),
        _ => {
            rewind(cursor, diagnostics)?;
            read_string_table(cursor, ctx, export, diagnostics, true)
        }
    }
}

/// `FStringTable::Serialize`: the namespace, then each entry as its key, its source string and,
/// as this game writes it, a marker string stock UE does not, then the metadata map keyed by
/// entry key: a count, then per record the key, an item count and the name and value pairs.
/// Strings are `FString`s, so a key or a value may be UTF-16.
fn read_string_table(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    export: u32,
    diagnostics: &mut Diagnostics,
    marker: bool,
) -> Result<StringTable, String> {
    let namespace_at = cursor.file_offset();
    let namespace = cursor.read_string()?;
    let count_at = cursor.file_offset();
    let count = read_count(cursor, "string table entry")?;
    let mut entries = Vec::with_capacity(count);
    let mut spans = Vec::with_capacity(count);
    for _ in 0..count {
        let key_at = cursor.file_offset();
        let key = cursor.read_string()?;
        let source_at = cursor.file_offset();
        let source = cursor.read_string()?;
        let tag_at = cursor.file_offset();
        let tag = if marker {
            cursor.read_string()?
        } else {
            diagnostics.stock_gaps.push(StockGap {
                export,
                at: tag_at,
                kind: StockGapKind::StringMarker { key: key.clone() },
            });
            String::new()
        };
        spans.push(StringEntrySpan {
            key: (key_at, source_at),
            source: (source_at, tag_at),
            tag_at,
            end: cursor.file_offset(),
        });
        entries.push(StringTableEntry {
            key,
            source,
            tag,
            metadata: Vec::new(),
        });
    }
    let trailer_at = cursor.file_offset();
    let mut loose_metadata = Vec::new();
    let mut records = Vec::new();
    for _ in 0..read_count(cursor, "string table metadata record")? {
        let start = cursor.file_offset();
        let key = cursor.read_string()?;
        let record_count_at = cursor.file_offset();
        let items = read_count(cursor, "string table metadata item")?;
        let mut pairs = Vec::with_capacity(items);
        let mut item_spans = Vec::with_capacity(items);
        for _ in 0..items {
            let item_at = cursor.file_offset();
            let id = cursor.read_name(ctx.names())?;
            let value_at = cursor.file_offset();
            let value = cursor.read_string()?;
            item_spans.push(MetaItemSpan {
                id: id.clone(),
                start: item_at,
                value: (value_at, cursor.file_offset()),
            });
            pairs.push((id, value));
        }
        records.push(MetaRecordSpan {
            key: key.clone(),
            start,
            count_at: record_count_at,
            items: item_spans,
            end: cursor.file_offset(),
        });
        match entries.iter_mut().find(|entry| entry.key == key) {
            Some(entry) => entry.metadata.extend(pairs),
            None => loose_metadata.push(StringTableMetaData { key, pairs }),
        }
    }
    if !marker {
        return Ok(StringTable {
            namespace,
            entries,
            loose_metadata,
        });
    }
    diagnostics.string_tables.push(StringTableLayout {
        export,
        namespace: (namespace_at, count_at),
        count_at,
        entries: spans,
        trailer_at,
        records,
        end: cursor.file_offset(),
    });
    Ok(StringTable {
        namespace,
        entries,
        loose_metadata,
    })
}

fn read_count(cursor: &mut Cursor<'_>, what: &str) -> Result<usize, String> {
    let count = cursor.read_i32()?;
    if count < 0 || count as usize > cursor.remaining() {
        return Err(cursor.err(format!("implausible {what} count {count}")));
    }
    Ok(count as usize)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use retoc::legacy_asset::{FLegacyPackageHeader, FPackageNameMap};

    use super::*;

    fn string(out: &mut Vec<u8>, text: &str) {
        out.extend_from_slice(&((text.len() + 1) as i32).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    }

    fn wide(out: &mut Vec<u8>, units: &[u16]) {
        out.extend_from_slice(&(-((units.len() + 1) as i32)).to_le_bytes());
        for unit in units {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }

    #[test]
    fn a_table_reads_its_entries_and_records_where_each_string_sits() {
        let mut data = Vec::new();
        string(&mut data, "104_Currency_ST");
        data.extend_from_slice(&2i32.to_le_bytes());
        string(&mut data, "Title");
        wide(&mut data, &[0x91D1, 0x5E01]);
        string(&mut data, "Encrypt");
        let second_key_at = data.len() as u64;
        string(&mut data, "Body");
        string(&mut data, "Plain");
        data.extend_from_slice(&0i32.to_le_bytes());
        let trailer_at = data.len() as u64;
        data.extend_from_slice(&2i32.to_le_bytes());
        string(&mut data, "Title");
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        string(&mut data, "from the map");
        string(&mut data, "Gone");
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        string(&mut data, "orphan");

        let header = FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(vec!["None".into(), "Comment".into()]),
            ..Default::default()
        };
        let ctx = Ctx {
            mappings: None,
            header: &header,
            fixups: None,
            synth: None,
            local: None,
            tagged: false,
        };
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(&data, 0);
        let table = read_string_table(&mut cursor, &ctx, 3, &mut diagnostics, true).expect("table");
        assert_eq!(cursor.remaining(), 0);
        assert_eq!(table.namespace, "104_Currency_ST");
        assert_eq!(table.entries.len(), 2);
        assert_eq!(table.entries[0].key, "Title");
        assert_eq!(table.entries[0].source, "\u{91D1}\u{5E01}");
        assert_eq!(table.entries[0].tag, "Encrypt");
        assert_eq!(
            table.entries[0].metadata,
            vec![("Comment".to_string(), "from the map".to_string())],
            "the metadata map's record joins the entry it keys on"
        );
        assert_eq!(table.entries[1].source, "Plain");
        assert_eq!(table.entries[1].tag, "");
        assert_eq!(table.loose_metadata.len(), 1);
        assert_eq!(table.loose_metadata[0].key, "Gone");

        let layout = &diagnostics.string_tables[0];
        assert_eq!(layout.export, 3);
        assert_eq!(layout.count_at, 4 + 16);
        assert_eq!(layout.entries[1].key.0, second_key_at);
        assert_eq!(layout.entries[1].end, trailer_at);
        assert_eq!(layout.trailer_at, trailer_at);
        assert_eq!(layout.entries[0].source.1, layout.entries[0].tag_at);
        assert_eq!(layout.records.len(), 2);
        assert_eq!(layout.records[0].key, "Title");
        assert_eq!(layout.records[0].start, trailer_at + 4);
        assert_eq!(layout.records[0].items.len(), 1);
        assert_eq!(layout.records[0].items[0].id, "Comment");
        assert_eq!(
            layout.records[0].items[0].value.1, layout.records[0].end,
            "the record ends with its last value"
        );
        assert_eq!(layout.records[1].start, layout.records[0].end);
        assert_eq!(layout.end, data.len() as u64);
    }

    #[test]
    fn a_negative_entry_count_is_refused() {
        let mut data = Vec::new();
        string(&mut data, "NS");
        data.extend_from_slice(&(-1i32).to_le_bytes());
        let header = FLegacyPackageHeader::default();
        let ctx = Ctx {
            mappings: None,
            header: &header,
            fixups: None,
            synth: None,
            local: None,
            tagged: false,
        };
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(&data, 0);
        let err =
            read_string_table(&mut cursor, &ctx, 0, &mut diagnostics, true).expect_err("refused");
        assert!(err.contains("-1"), "{err}");
    }

    /// A stock table has no marker after each source. Read as this game's layout its second key
    /// is taken for the first marker and the table overruns, so it reads as a stock one: what it
    /// holds, where each marker goes, and no layout to edit through.
    #[test]
    fn a_stock_table_reads_with_its_missing_markers_recorded() {
        let mut data = Vec::new();
        string(&mut data, "NS");
        data.extend_from_slice(&2i32.to_le_bytes());
        string(&mut data, "Title");
        string(&mut data, "Hello");
        let first_gap = data.len() as u64;
        string(&mut data, "Body");
        string(&mut data, "World");
        let second_gap = data.len() as u64;
        data.extend_from_slice(&0i32.to_le_bytes());
        let header = FLegacyPackageHeader::default();
        let ctx = Ctx {
            mappings: None,
            header: &header,
            fixups: None,
            synth: None,
            local: None,
            tagged: false,
        };
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(&data, 0);
        let table = read_either_layout(&mut cursor, &ctx, 4, &mut diagnostics).expect("read");
        assert_eq!(cursor.remaining(), 0);
        let read: Vec<(&str, &str)> = table
            .entries
            .iter()
            .map(|entry| (entry.key.as_str(), entry.source.as_str()))
            .collect();
        assert_eq!(read, [("Title", "Hello"), ("Body", "World")]);
        let gaps: Vec<(u32, u64)> = diagnostics
            .stock_gaps
            .iter()
            .map(|gap| (gap.export, gap.at))
            .collect();
        assert_eq!(gaps, [(4, first_gap), (4, second_gap)]);
        assert!(diagnostics.string_tables.is_empty());
    }
}
