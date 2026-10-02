//! A text written the way UE writes one out: `LOCTABLE("/Game/UI/X_ST.X_ST", "Key")`,
//! `NSLOCTEXT("Namespace", "Key", "Source")`, `INVTEXT("Text")`, or `LOCGEN_TOUPPER(...)` /
//! `LOCGEN_TOLOWER(...)` around one of those. It names the kind of text as well as what it shows,
//! which a plain string cannot: typing one is how a text changes kind, such as a label pointed at
//! another string table entry, or taken off its table to show fixed text.

use crate::value::PropertyValue;

/// A text as its literal spells it, one variant per history the editor can build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextLiteral {
    /// `INVTEXT`: shown the same in every language (history None, culture invariant).
    Invariant(String),
    /// `NSLOCTEXT`: a source string localized under a namespace and key (history Base).
    Localized {
        namespace: String,
        key: String,
        source: String,
    },
    /// `LOCTABLE`: whatever a string table holds under a key (history StringTableEntry).
    Table { table_id: String, key: String },
    /// `LOCGEN_TOUPPER` / `LOCGEN_TOLOWER`: another text, upper- or lower-cased (history Transform).
    Transform {
        upper: bool,
        inner: Box<TextLiteral>,
    },
}

/// The macros that spell a text UE can write but this editor cannot build.
const UNSUPPORTED: &[(&str, &str)] = &[
    (
        "LOCTEXT",
        "name its namespace with NSLOCTEXT(\"Namespace\", \"Key\", \"Source\")",
    ),
    (
        "LOCGEN_",
        "only LOCGEN_TOUPPER and LOCGEN_TOLOWER can be built",
    ),
];

const FORMS: &str = "LOCTABLE(\"Table\", \"Key\"), NSLOCTEXT(\"Namespace\", \"Key\", \"Source\"), \
                     INVTEXT(\"Text\"), or LOCGEN_TOUPPER/LOCGEN_TOLOWER around one of them";

/// The literal a text spells, `None` when it is a plain string, or why it cannot be built. Only a
/// known macro name followed by `(` makes a literal, so `Hello (again)` stays plain.
pub fn parse(text: &str) -> Option<Result<TextLiteral, String>> {
    let trimmed = text.trim();
    let name_end = trimmed
        .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
        .unwrap_or(trimmed.len());
    let name = &trimmed[..name_end];
    if name.is_empty() || !trimmed[name_end..].trim_start().starts_with('(') {
        return None;
    }
    let known = matches!(
        name,
        "INVTEXT" | "NSLOCTEXT" | "LOCTABLE" | "LOCGEN_TOUPPER" | "LOCGEN_TOLOWER"
    );
    if !known {
        let refused = UNSUPPORTED.iter().find(|(prefix, _)| {
            name == *prefix || (prefix.ends_with('_') && name.starts_with(prefix))
        });
        return refused.map(|(_, why)| {
            Err(format!(
                "{name}(...) cannot be built here: {why}. A text can be {FORMS}"
            ))
        });
    }
    let mut parser = Parser {
        text: trimmed,
        at: 0,
    };
    Some(parser.literal().and_then(|literal| {
        parser.skip_space();
        if parser.at == parser.text.len() {
            Ok(literal)
        } else {
            Err(format!(
                "{} follows the text's closing bracket; a text is one of {FORMS}",
                &parser.text[parser.at..]
            ))
        }
    }))
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn skip_space(&mut self) {
        let rest = &self.text[self.at..];
        self.at += rest.len() - rest.trim_start().len();
    }

    fn name(&mut self) -> &str {
        self.skip_space();
        let rest = &self.text[self.at..];
        let len = rest
            .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .unwrap_or(rest.len());
        self.at += len;
        &rest[..len]
    }

    fn expect(&mut self, wanted: char) -> Result<(), String> {
        self.skip_space();
        if self.text[self.at..].starts_with(wanted) {
            self.at += wanted.len_utf8();
            Ok(())
        } else {
            Err(format!(
                "expected {wanted} at \"{}\"",
                self.text[self.at..].chars().take(24).collect::<String>()
            ))
        }
    }

    fn literal(&mut self) -> Result<TextLiteral, String> {
        let name = self.name().to_string();
        self.expect('(')?;
        let literal = match name.as_str() {
            "INVTEXT" => TextLiteral::Invariant(self.string()?),
            "NSLOCTEXT" => {
                let namespace = self.string()?;
                self.expect(',')?;
                let key = self.string()?;
                self.expect(',')?;
                TextLiteral::Localized {
                    namespace,
                    key,
                    source: self.string()?,
                }
            }
            "LOCTABLE" => {
                let table_id = self.string()?;
                self.expect(',')?;
                TextLiteral::Table {
                    table_id,
                    key: self.string()?,
                }
            }
            "LOCGEN_TOUPPER" | "LOCGEN_TOLOWER" => TextLiteral::Transform {
                upper: name == "LOCGEN_TOUPPER",
                inner: Box::new(self.literal()?),
            },
            other => {
                return Err(format!(
                    "{other}(...) is not a text; a text is one of {FORMS}"
                ));
            }
        };
        self.expect(')')?;
        Ok(literal)
    }

    /// A quoted string, with the escapes UE writes: `\"`, `\\`, `\n`, `\r` and `\t`.
    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        let mut chars = self.text[self.at..].char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    self.at += i + 1;
                    return Ok(out);
                }
                '\\' => match chars.next() {
                    Some((_, '"')) => out.push('"'),
                    Some((_, '\\')) => out.push('\\'),
                    Some((_, 'n')) => out.push('\n'),
                    Some((_, 'r')) => out.push('\r'),
                    Some((_, 't')) => out.push('\t'),
                    Some((_, other)) => {
                        return Err(format!(
                            "\\{other} is not an escape a text takes; write \\\\ for a backslash"
                        ));
                    }
                    None => break,
                },
                other => out.push(other),
            }
        }
        Err("a quoted string in the text is never closed".to_string())
    }
}

fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// The literal as UE writes it.
pub fn format(literal: &TextLiteral) -> String {
    match literal {
        TextLiteral::Invariant(text) => format!("INVTEXT({})", quoted(text)),
        TextLiteral::Localized {
            namespace,
            key,
            source,
        } => format!(
            "NSLOCTEXT({}, {}, {})",
            quoted(namespace),
            quoted(key),
            quoted(source)
        ),
        TextLiteral::Table { table_id, key } => {
            format!("LOCTABLE({}, {})", quoted(table_id), quoted(key))
        }
        TextLiteral::Transform { upper, inner } => format!(
            "{}({})",
            if *upper {
                "LOCGEN_TOUPPER"
            } else {
                "LOCGEN_TOLOWER"
            },
            format(inner)
        ),
    }
}

/// The literal a decoded text reads as, `None` for anything that is not a text. A None text and a
/// formatted number both read as `INVTEXT` of what they show, since the decoded value keeps no
/// history; the encoder works from the bytes, so only this reading is approximate.
pub fn of_value(value: &PropertyValue) -> Option<TextLiteral> {
    let PropertyValue::Text {
        value,
        parts,
        namespace,
        key,
        ..
    } = value
    else {
        return None;
    };
    let part = |name: &str| parts.iter().find(|part| part.name == name);
    if let (Some(table), Some(entry)) = (part("TableId"), part("Key")) {
        let (PropertyValue::Name { value: table_id }, PropertyValue::Str { value: key }) =
            (&table.value, &entry.value)
        else {
            return None;
        };
        return Some(TextLiteral::Table {
            table_id: table_id.clone(),
            key: key.clone(),
        });
    }
    if let (Some(source), Some(kind)) = (part("SourceText"), part("TransformType")) {
        let PropertyValue::Byte { value: kind } = kind.value else {
            return None;
        };
        return Some(TextLiteral::Transform {
            upper: kind == 1,
            inner: Box::new(of_value(&source.value)?),
        });
    }
    // A pattern, a number, a moment or a generator: no literal spells one.
    if !parts.is_empty() {
        return None;
    }
    let shown = value.clone().unwrap_or_default();
    Some(match namespace {
        Some(namespace) => TextLiteral::Localized {
            namespace: namespace.clone(),
            key: key.clone().unwrap_or_default(),
            source: shown,
        },
        None => TextLiteral::Invariant(shown),
    })
}

/// Whether a decoded text is the one a literal spells.
pub fn matches(literal: &TextLiteral, value: &PropertyValue) -> bool {
    of_value(value).as_ref() == Some(literal)
}

/// The table and key a plain string names when it is typed over a string table text in the form
/// that text reads as, `Table:Key`. The table has to be the one the text uses now or an asset path,
/// so a sentence that happens to hold a colon is not taken for one; the first colon splits, since
/// an asset path holds none and a key may.
pub fn table_reference(text: &str, current_table: &str) -> Option<(String, String)> {
    let (table, key) = text.split_once(':')?;
    if key.is_empty() {
        return None;
    }
    let asset_path = table.starts_with('/')
        && table.contains('.')
        && !table.contains(|c: char| c.is_whitespace() || c == '"');
    (table == current_table || asset_path).then(|| (table.to_string(), key.to_string()))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::value::PropertyEntry;

    fn literal(text: &str) -> TextLiteral {
        parse(text).expect("a literal").expect("well formed")
    }

    fn refused(text: &str) -> String {
        parse(text).expect("a literal").expect_err("refused")
    }

    /// Every form reads back as what it spells, and writes out the way UE does.
    #[test]
    fn every_form_parses_and_writes_back_the_same() {
        for (text, expected) in [
            (
                r#"LOCTABLE("/Game/UI/X_ST.X_ST", "Key")"#,
                TextLiteral::Table {
                    table_id: "/Game/UI/X_ST.X_ST".into(),
                    key: "Key".into(),
                },
            ),
            (
                r#"NSLOCTEXT("Menu", "Play", "Play now")"#,
                TextLiteral::Localized {
                    namespace: "Menu".into(),
                    key: "Play".into(),
                    source: "Play now".into(),
                },
            ),
            (
                r#"INVTEXT("Fixed")"#,
                TextLiteral::Invariant("Fixed".into()),
            ),
            (
                r#"LOCGEN_TOUPPER(LOCTABLE("/Game/T.T", "K"))"#,
                TextLiteral::Transform {
                    upper: true,
                    inner: Box::new(TextLiteral::Table {
                        table_id: "/Game/T.T".into(),
                        key: "K".into(),
                    }),
                },
            ),
            (
                r#"LOCGEN_TOLOWER(INVTEXT("Loud"))"#,
                TextLiteral::Transform {
                    upper: false,
                    inner: Box::new(TextLiteral::Invariant("Loud".into())),
                },
            ),
        ] {
            assert_eq!(literal(text), expected, "{text}");
            assert_eq!(format(&expected), text);
        }
    }

    /// Space around the pieces is allowed, and the escapes UE writes read back as the characters
    /// they stand for, so a formatted literal always parses to itself.
    #[test]
    fn spacing_and_escapes_are_read_as_ue_writes_them() {
        assert_eq!(
            literal("  INVTEXT (  \"a\\\"b\\\\c\\nd\\te\"  )  "),
            TextLiteral::Invariant("a\"b\\c\nd\te".into())
        );
        let tricky = TextLiteral::Invariant("quote \" slash \\ line\nend".into());
        assert_eq!(literal(&format(&tricky)), tricky);
    }

    /// Only a known macro name and a bracket make a literal; anything else is a plain string.
    #[test]
    fn a_plain_string_is_not_a_literal() {
        for text in [
            "Hello",
            "Hello (again)",
            "INVTEXT is a macro",
            "invtext(\"lower case\")",
            "/Game/T.T:Key",
            "",
        ] {
            assert!(parse(text).is_none(), "{text}");
        }
    }

    /// A literal that goes wrong once it has begun is refused with the reason, not written as
    /// a plain string that happens to look like one.
    #[test]
    fn a_malformed_literal_is_refused() {
        assert!(refused(r#"INVTEXT("open"#).contains("never closed"));
        assert!(refused(r#"INVTEXT("a\qb")"#).contains("\\q"));
        assert!(refused(r#"LOCTABLE("T")"#).contains("expected ,"));
        assert!(refused(r#"INVTEXT("a") extra"#).contains("follows"));
        assert!(refused(r#"LOCTEXT("K", "S")"#).contains("NSLOCTEXT"));
        assert!(refused(r#"LOCGEN_NUMBER(3, "")"#).contains("TOUPPER"));
    }

    fn text(
        value: Option<&str>,
        parts: Vec<PropertyEntry>,
        namespace: Option<&str>,
    ) -> PropertyValue {
        PropertyValue::Text {
            value: value.map(str::to_string),
            parts,
            namespace: namespace.map(str::to_string),
            key: namespace.map(|_| "K".to_string()),
            display: None,
        }
    }

    fn part(name: &str, value: PropertyValue) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            value,
            span: None,
            slot: None,
        }
    }

    fn table_text(table: &str, key: &str) -> PropertyValue {
        text(
            Some(&format!("{table}:{key}")),
            vec![
                part(
                    "TableId",
                    PropertyValue::Name {
                        value: table.into(),
                    },
                ),
                part("Key", PropertyValue::Str { value: key.into() }),
            ],
            None,
        )
    }

    /// A decoded text reads as the literal that would rebuild it, down through a transform.
    #[test]
    fn a_decoded_text_reads_as_its_literal() {
        let table = table_text("/Game/T.T", "K");
        assert_eq!(
            of_value(&table),
            Some(TextLiteral::Table {
                table_id: "/Game/T.T".into(),
                key: "K".into()
            })
        );
        let upper = text(
            Some("/Game/T.T:K"),
            vec![
                part("SourceText", table.clone()),
                part("TransformType", PropertyValue::Byte { value: 1 }),
            ],
            None,
        );
        assert_eq!(
            format(&of_value(&upper).expect("a transform")),
            r#"LOCGEN_TOUPPER(LOCTABLE("/Game/T.T", "K"))"#
        );
        assert_eq!(
            of_value(&text(Some("Hi"), vec![], Some("NS"))),
            Some(TextLiteral::Localized {
                namespace: "NS".into(),
                key: "K".into(),
                source: "Hi".into()
            })
        );
        assert_eq!(
            of_value(&text(None, vec![], None)),
            Some(TextLiteral::Invariant(String::new()))
        );
        assert!(matches(&literal(r#"LOCTABLE("/Game/T.T", "K")"#), &table));
        assert!(!matches(
            &literal(r#"LOCTABLE("/Game/T.T", "Other")"#),
            &table
        ));
        assert!(of_value(&PropertyValue::Str { value: "x".into() }).is_none());
    }

    /// `Table:Key` typed over a table text repoints it, but only when the table is the one it
    /// uses or an asset path, so a sentence with a colon stays a sentence.
    #[test]
    fn a_table_and_key_is_read_only_where_it_cannot_be_a_sentence() {
        assert_eq!(
            table_reference("/Game/A/B_ST.B_ST:Key:x", "/Game/Other.Other"),
            Some(("/Game/A/B_ST.B_ST".into(), "Key:x".into()))
        );
        assert_eq!(
            table_reference("MyTable:Key", "MyTable"),
            Some(("MyTable".into(), "Key".into()))
        );
        assert!(table_reference("Hello: world", "/Game/T.T").is_none());
        assert!(table_reference("/Game/T.T:", "/Game/T.T").is_none());
        assert!(table_reference("/not a path.x:Key", "/Game/T.T").is_none());
    }
}
