//! Reads the row block a UDataTable writes after its own properties.

use serde::Serialize;

use crate::props::{Ctx, Diagnostics, MissingSchema, read_property_block};
use crate::reader::Cursor;
use crate::tagged::read_tagged_block;
use crate::value::{PropertyEntry, PropertyValue};

#[derive(Debug, Clone, Serialize)]
pub struct DataTable {
    pub row_struct: String,
    /// Column labels in row-schema order, so an empty table still renders its shape. These match
    /// `PropertyEntry::label`, which is what lets a consumer join a row to its columns.
    pub columns: Vec<String>,
    pub rows: Vec<DataTableRow>,
    /// How many rows the table declared, which exceeds `rows.len()` when parsing stopped early.
    pub declared_rows: u32,
    /// Why parsing stopped before the last row. The rows above it are still trustworthy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DataTableRow {
    pub name: String,
    pub fields: Vec<PropertyEntry>,
}

/// Where a table's rows sit, for edits that add, drop or rename one.
#[derive(Debug, Clone)]
pub struct DataTableLayout {
    pub export: u32,
    /// The `i32` holding how many rows follow.
    pub count_at: u64,
    /// How many slots the row struct declares, which is what an empty row's header has to cover.
    pub row_slots: usize,
    /// Rows are tagged property blocks, so an empty one is a lone `None` rather than a header.
    pub tagged: bool,
    /// One span per decoded row, in table order.
    pub rows: Vec<RowSpan>,
}

/// A row's bytes: its name, then its property block.
#[derive(Debug, Clone, Copy)]
pub struct RowSpan {
    pub start: u64,
    pub end: u64,
}

pub(crate) fn read_rows(
    cursor: &mut Cursor<'_>,
    properties: &[PropertyEntry],
    ctx: &Ctx<'_>,
    export: u32,
    tagged: bool,
    diagnostics: &mut Diagnostics,
) -> Result<DataTable, String> {
    let path = row_struct_path(properties)
        .ok_or_else(|| cursor.err("DataTable has no resolvable RowStruct"))?;
    let row_struct = last_path_segment(&path);
    // Tagged rows describe themselves, so the schema only names the columns and is not required.
    let schema = match ctx.schema(&row_struct) {
        Some(schema) => Some(schema),
        None if tagged => None,
        None => {
            // A Blueprint row struct lives in its own package, so record where it is: the caller
            // can read the definition from there and parse again.
            diagnostics.missing_schemas.push(MissingSchema {
                name: row_struct.clone(),
                object_path: path,
            });
            return Err(cursor.err(format!(
                "row struct {row_struct} is not in the mappings file"
            )));
        }
    };

    let count_at = cursor.file_offset();
    let count = cursor.read_i32()?;
    if count < 0 || count as usize > cursor.remaining() {
        return Err(cursor.err(format!("implausible DataTable row count {count}")));
    }

    // A row that fails leaves every row above it intact, so keep them and say where it stopped.
    let mut rows = Vec::with_capacity(count as usize);
    let mut spans = Vec::with_capacity(count as usize);
    let mut truncated = None;
    for index in 0..count {
        let start = cursor.file_offset();
        let name = match cursor.read_name(ctx.names()) {
            Ok(name) => name,
            Err(reason) => {
                truncated = Some(format!("row {index}: {reason}"));
                break;
            }
        };
        let mut fields = Vec::new();
        let outcome = match &schema {
            Some(schema) if !tagged => {
                read_property_block(cursor, schema, ctx, diagnostics, 0, &mut fields)
            }
            _ => read_tagged_block(cursor, ctx, diagnostics, 0, &mut fields, Some(&row_struct)),
        };
        let failed = outcome.err().map(|e| format!("row {index} ({name}): {e}"));
        rows.push(DataTableRow { name, fields });
        spans.push(RowSpan {
            start,
            end: cursor.file_offset(),
        });
        if let Some(reason) = failed {
            truncated = Some(reason);
            break;
        }
    }
    diagnostics.tables.push(DataTableLayout {
        export,
        count_at,
        row_slots: if tagged {
            0
        } else {
            schema.as_ref().map_or(0, |s| s.len())
        },
        tagged,
        rows: spans,
    });

    let columns = match &schema {
        Some(schema) => schema
            .iter()
            .map(|s| {
                if s.property.array_dim > 1 {
                    format!("{}[{}]", s.property.name, s.element)
                } else {
                    s.property.name.clone()
                }
            })
            .collect(),
        None => decoded_columns(&rows),
    };
    Ok(DataTable {
        columns,
        row_struct,
        rows,
        declared_rows: count as u32,
        truncated,
    })
}

/// Without a schema, the columns are every field the rows stored, in the order first seen.
fn decoded_columns(rows: &[DataTableRow]) -> Vec<String> {
    let mut columns: Vec<String> = Vec::new();
    for field in rows.iter().flat_map(|row| &row.fields) {
        let label = field.label();
        if !columns.contains(&label) {
            columns.push(label);
        }
    }
    columns
}

/// `RowStruct` is an object property pointing at the UScriptStruct describing every row.
fn row_struct_path(properties: &[PropertyEntry]) -> Option<String> {
    let entry = properties.iter().find(|p| p.name == "RowStruct")?;
    match &entry.value {
        PropertyValue::Object { path, .. } => path.clone(),
        _ => None,
    }
}

fn last_path_segment(path: &str) -> String {
    path.rsplit(['/', '.', ':'])
        .next()
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn object(name: &str, path: Option<&str>) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            span: None,
            slot: None,
            value: PropertyValue::Object {
                index: -1,
                path: path.map(str::to_string),
            },
        }
    }

    /// The whole path is kept so a Blueprint row struct can be traced back to the package that
    /// defines it, while the schema is still looked up by the bare name.
    #[test]
    fn the_row_struct_keeps_its_path_and_resolves_to_the_last_segment() {
        let properties = vec![object("RowStruct", Some("/Script/Marvel.HeroAttributeRow"))];
        let path = row_struct_path(&properties).expect("row struct");
        assert_eq!(path, "/Script/Marvel.HeroAttributeRow");
        assert_eq!(last_path_segment(&path), "HeroAttributeRow");
    }

    #[test]
    fn a_table_with_no_row_struct_property_yields_nothing_rather_than_a_guess() {
        assert!(row_struct_path(&[object("Other", Some("/Script/X.Y"))]).is_none());
    }

    #[test]
    fn a_null_row_struct_reference_yields_nothing() {
        assert!(row_struct_path(&[object("RowStruct", None)]).is_none());
    }
}
