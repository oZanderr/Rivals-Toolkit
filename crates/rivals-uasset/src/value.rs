//! The decoded property representation shared by the desktop UI and the CLI.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct PropertyEntry {
    pub name: String,
    /// Set only for static array properties, where one name covers several slots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element: Option<u32>,
    pub value: PropertyValue,
    /// Where the value sits in the package. An empty range means the unversioned header flagged it
    /// as holding its default, so it is written nowhere and that offset is where it would go.
    /// `None` only where the reader does not track offsets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<(u64, u64)>,
    /// Which flag in the enclosing block's header governs this value, for edits that store a
    /// defaulted value or send a stored one back to its default.
    #[serde(skip)]
    pub slot: Option<SlotRef>,
}

/// Addresses one property's flag in the unversioned header of the block that holds it.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SlotRef {
    /// File offset of the block header, which is where an edit re-emits it.
    pub header_at: u64,
    /// The property's slot in the flattened schema, which is how the header names it whether or
    /// not it currently holds a value.
    pub schema_index: u32,
    /// The type the property is stored as, which is the only thing that says how wide a value
    /// should be when it holds its default and so has no bytes to measure.
    pub declared: &'static str,
}

impl PropertyEntry {
    /// Display name. A static array declares one property across several slots, so the element
    /// index has to be part of the label or the columns collide.
    pub fn label(&self) -> String {
        match self.element {
            Some(index) => format!("{}[{index}]", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MapEntry {
    pub key: PropertyValue,
    pub value: PropertyValue,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PropertyValue {
    Bool {
        value: bool,
    },
    Int {
        value: i64,
    },
    UInt {
        value: u64,
    },
    Float {
        value: f64,
    },
    Byte {
        value: u8,
    },
    Str {
        value: String,
    },
    Name {
        value: String,
    },
    Text {
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        /// The pieces a formatted, dated or transformed text is built from, each editable on its
        /// own where the text as a whole is not.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        parts: Vec<PropertyEntry>,
    },
    Enum {
        value: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The enum's type, for offering its enumerators and for writing one back by name.
        #[serde(skip_serializing_if = "Option::is_none")]
        enum_type: Option<String>,
    },
    Object {
        index: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    SoftObject {
        path: String,
    },
    /// A bound UFunction. Kept distinct from a plain string so a delegate cannot be mistaken for
    /// text in the property tree, in CSV output or in the JSON.
    Delegate {
        #[serde(skip_serializing_if = "Option::is_none")]
        object: Option<String>,
        function: String,
    },
    FieldPath {
        path: String,
    },
    LazyObject {
        guid: String,
    },
    Array {
        items: Vec<PropertyValue>,
    },
    Set {
        items: Vec<PropertyValue>,
    },
    Map {
        entries: Vec<MapEntry>,
    },
    Struct {
        name: String,
        fields: Vec<PropertyEntry>,
    },
    /// An instanced struct payload the reader could not decode. Its bytes are kept exactly as
    /// they are; it is a value of its own so nothing can mistake the reason for a stored string.
    Undecoded {
        reason: String,
        /// The payload's length, which is what still identifies it once an edit has moved the
        /// offsets its reason names.
        bytes: u64,
    },
    /// The property was covered by the header zero mask, so it holds its default value and
    /// occupies no bytes. Kept distinct from a real zero so the UI never implies a stored value.
    Default,
    /// The header skips this slot: the export stores nothing for it and the object keeps the value
    /// it inherits from its archetype, which the reader cannot see. `declared` is the type it is
    /// stored as, which is all an edit has to size a value by.
    Unset {
        declared: &'static str,
        /// The enum type behind an enum slot's declared integer, so a value can be typed by name.
        #[serde(skip_serializing_if = "Option::is_none")]
        enum_type: Option<String>,
        /// For an unset struct, the fields its schema declares, unset in turn. They have no bytes
        /// yet, so they are shown and addressed through the struct: see [`crate::FieldSet`].
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<PropertyEntry>,
    },
}

impl PropertyValue {
    /// A short one-line rendering for tables and CLI output.
    pub fn summary(&self) -> String {
        match self {
            Self::Bool { value } => value.to_string(),
            Self::Int { value } => value.to_string(),
            Self::UInt { value } => value.to_string(),
            Self::Float { value } => format_float(*value),
            Self::Byte { value } => value.to_string(),
            Self::Str { value } => value.clone(),
            Self::Name { value } => value.clone(),
            Self::Text { value, .. } => value.clone().unwrap_or_default(),
            Self::Enum { value, name, .. } => name.clone().unwrap_or_else(|| value.to_string()),
            Self::Object { index, path } => path.clone().unwrap_or_else(|| {
                if *index == 0 {
                    "None".into()
                } else {
                    index.to_string()
                }
            }),
            Self::SoftObject { path } => path.clone(),
            Self::Delegate { object, function } => match object {
                Some(object) => format!("{object}::{function}"),
                None => function.clone(),
            },
            Self::FieldPath { path } => path.clone(),
            Self::LazyObject { guid } => guid.clone(),
            Self::Array { items } => format!("[{} items]", items.len()),
            Self::Set { items } => format!("{{{} items}}", items.len()),
            Self::Map { entries } => format!("{{{} entries}}", entries.len()),
            Self::Struct { name, fields } => {
                if let Some(inline) = inline_struct(fields) {
                    format!("{name}({inline})")
                } else {
                    format!("{name} {{{} fields}}", fields.len())
                }
            }
            Self::Undecoded { reason, bytes } => format!("({bytes} bytes not decoded: {reason})"),
            Self::Default => "(default)".into(),
            Self::Unset { .. } => "(not stored)".into(),
        }
    }
}

/// Vectors and colours read far better on one line than as a nested tree.
fn inline_struct(fields: &[PropertyEntry]) -> Option<String> {
    if fields.is_empty() || fields.len() > 4 {
        return None;
    }
    let mut parts = Vec::with_capacity(fields.len());
    for field in fields {
        match &field.value {
            PropertyValue::Float { value } => parts.push(format_float(*value)),
            PropertyValue::Int { value } => parts.push(value.to_string()),
            PropertyValue::UInt { value } => parts.push(value.to_string()),
            PropertyValue::Byte { value } => parts.push(value.to_string()),
            PropertyValue::Unset { .. } => parts.push("-".into()),
            _ => return None,
        }
    }
    Some(parts.join(", "))
}

fn format_float(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn float_field(name: &str, value: f64) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            span: None,
            slot: None,
            value: PropertyValue::Float { value },
        }
    }

    #[test]
    fn a_small_numeric_struct_summarises_on_one_line() {
        let value = PropertyValue::Struct {
            name: "Vector".into(),
            fields: vec![
                float_field("X", 1.0),
                float_field("Y", 2.5),
                float_field("Z", 0.0),
            ],
        };
        assert_eq!(value.summary(), "Vector(1.0, 2.5, 0.0)");
    }

    #[test]
    fn a_struct_with_non_numeric_fields_falls_back_to_a_field_count() {
        let value = PropertyValue::Struct {
            name: "Row".into(),
            fields: vec![PropertyEntry {
                name: "Label".into(),
                element: None,
                span: None,
                slot: None,
                value: PropertyValue::Str {
                    value: "hello".into(),
                },
            }],
        };
        assert_eq!(value.summary(), "Row {1 fields}");
    }

    #[test]
    fn a_static_array_element_carries_its_index_in_the_label() {
        let entry = PropertyEntry {
            name: "LensFlareTints".into(),
            element: Some(3),
            span: None,
            slot: None,
            value: PropertyValue::Default,
        };
        assert_eq!(entry.label(), "LensFlareTints[3]");
    }

    #[test]
    fn a_scalar_property_labels_as_its_bare_name() {
        assert_eq!(
            PropertyEntry {
                name: "Damage".into(),
                element: None,
                span: None,
                slot: None,
                value: PropertyValue::Default,
            }
            .label(),
            "Damage"
        );
    }

    #[test]
    fn a_delegate_summarises_as_object_and_function_not_as_a_bare_string() {
        let value = PropertyValue::Delegate {
            object: Some("/Game/BP_Thing.BP_Thing_C".into()),
            function: "OnFired".into(),
        };
        assert_eq!(value.summary(), "/Game/BP_Thing.BP_Thing_C::OnFired");
    }

    #[test]
    fn an_unbound_delegate_summarises_as_just_its_function_name() {
        let value = PropertyValue::Delegate {
            object: None,
            function: "OnFired".into(),
        };
        assert_eq!(value.summary(), "OnFired");
    }

    #[test]
    fn a_defaulted_property_is_never_summarised_as_a_stored_zero() {
        assert_eq!(PropertyValue::Default.summary(), "(default)");
    }

    /// An undecoded payload says how much it holds and why, so it can never read as a value.
    #[test]
    fn an_undecoded_payload_summarises_as_its_length_and_its_reason() {
        let value = PropertyValue::Undecoded {
            reason: "no schema for Broken".into(),
            bytes: 42,
        };
        assert_eq!(
            value.summary(),
            "(42 bytes not decoded: no schema for Broken)"
        );
    }
}
