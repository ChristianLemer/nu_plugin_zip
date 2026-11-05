//! `to zip`: a table of `name` and `content`, the shape `from zip` returns, written as an
//! archive in memory. Content that is neither a string nor binary goes through the
//! `to <ext>` its name's extension picks, as `save` would.

use std::io::Cursor;

use chrono::{Local, NaiveDateTime};
use nu_plugin::{EngineInterface, EvaluatedCall, SimplePluginCommand};
use nu_protocol::{
    Category, Example, LabeledError, PipelineData, Signature, Span, SyntaxShape, Type, Value,
};

use crate::write::{check_mtime, Writer};
use crate::zip_open::extension;
use crate::ZipPlugin;

/// What one entry holds before it is written.
#[derive(Debug)]
pub(crate) enum Content {
    Bytes(Vec<u8>),
    /// A value still to be serialized by the entry's extension.
    Value(Value),
    Dir,
}

/// One entry to write, in input order.
#[derive(Debug)]
pub(crate) struct Item {
    /// As given; the `Writer` checks it.
    pub name: String,
    pub content: Content,
    pub modified: Option<NaiveDateTime>,
    /// Where the entry came from, for errors: `row 3`.
    pub origin: String,
}

pub struct ToZip;

#[allow(clippy::unnecessary_literal_bound)]
impl SimplePluginCommand for ToZip {
    type Plugin = ZipPlugin;

    fn name(&self) -> &str {
        "to zip"
    }

    fn description(&self) -> &str {
        "Write a table of name and content as zip bytes, the inverse of from zip"
    }

    fn extra_description(&self) -> &str {
        "This is what `save x.zip` calls. A string is written as UTF-8, binary as is; any \
         other content goes through the `to` command its extension names, so a.nuon goes \
         through `to nuon`. A name ending in / is a directory. Parents get their own \
         entries. `modified` is read when present; other columns are ignored. The archive \
         is built in memory; nothing is written to disk. Binary input is taken to be an \
         archive already and comes back unchanged, so `to zip | save x.zip` works."
    }

    fn signature(&self) -> Signature {
        Signature::build("to zip")
            .input_output_types(vec![
                (Type::table(), Type::Binary),
                (Type::list(Type::Any), Type::Binary),
                (Type::Nothing, Type::Binary),
                (Type::Binary, Type::Binary),
            ])
            .named(
                "mtime",
                SyntaxShape::DateTime,
                "Give every entry this date, for byte-identical output",
                None,
            )
            .category(Category::Formats)
    }

    fn search_terms(&self) -> Vec<&str> {
        vec!["zip", "archive", "write", "compress"]
    }

    fn examples(&self) -> Vec<Example<'_>> {
        vec![
            Example {
                description: "An archive holding one file, in its directory",
                example: "[[name content]; [notes/a.txt hello]] | to zip",
                result: None,
            },
            Example {
                description: "Rewrite an archive through a table, as from zip returns it",
                example: "open archive.zip | to zip",
                result: None,
            },
            Example {
                description: "A table stored as nuon, with the same bytes on every run",
                example: "[[name content]; [a.nuon [[x]; [1]]]] | to zip --mtime 2026-01-01",
                result: None,
            },
        ]
    }

    fn run(
        &self,
        _plugin: &ZipPlugin,
        engine: &EngineInterface,
        call: &EvaluatedCall,
        input: &Value,
    ) -> Result<Value, LabeledError> {
        let span = call.head;
        let now = Local::now().naive_local();
        let mtime = match call.get_flag_value("mtime") {
            Some(v) => Some(check_mtime(v.as_date()?.naive_local()).map_err(|e| {
                LabeledError::new(e.to_string()).with_label("outside what a zip holds", v.span())
            })?),
            None => None,
        };
        let fail = |e: LabeledError| e.with_label("refused here", call.head);
        let rows = match input {
            // `save x.zip` sends a binary value through `to zip` too, so the bytes of
            // `... | to zip | save x.zip` arrive here a second time: they are the archive.
            // `--mtime` has no effect on them.
            Value::Binary { .. } => return Ok(input.clone()),
            Value::Nothing { .. } => &[][..],
            Value::List { vals, .. } => vals.as_slice(),
            other => {
                return Err(LabeledError::new(format!(
                    "expected a table of name and content, got {}",
                    other.get_type()
                ))
                .with_label("not a table", other.span()))
            }
        };
        let items = from_rows(rows).map_err(fail)?;
        let items = serialize(engine, items, span).map_err(fail)?;
        let bytes = build(&items, mtime, now).map_err(fail)?;
        Ok(Value::binary(bytes, span))
    }
}

/// The rows as items, in order. Only `name`, `content` and `modified` are read.
pub(crate) fn from_rows(rows: &[Value]) -> Result<Vec<Item>, LabeledError> {
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let origin = format!("row {i}");
            let bad = |msg: String| LabeledError::new(format!("{origin}: {msg}"));
            let record = row
                .as_record()
                .map_err(|_| bad(format!("expected a record, got {}", row.get_type())))?;
            let name = match record.get("name") {
                Some(Value::String { val, .. }) => val.clone(),
                Some(v) => {
                    return Err(bad(format!(
                        "`name` must be a string, not {}",
                        v.get_type()
                    )))
                }
                None => return Err(bad("no `name` column".into())),
            };
            let content = if name.ends_with('/') {
                match record.get("content") {
                    None | Some(Value::Nothing { .. }) => Content::Dir,
                    Some(Value::String { val, .. }) if val.is_empty() => Content::Dir,
                    Some(v @ Value::Binary { .. }) if binary_bytes(v).is_empty() => Content::Dir,
                    Some(_) => {
                        return Err(bad(format!(
                            "`{name}` is a directory and cannot hold content"
                        )))
                    }
                }
            } else {
                // A file without content is refused rather than written empty: a missing
                // column or a null is far more often a mistake upstream than a wish.
                match record.get("content") {
                    None | Some(Value::Nothing { .. }) => {
                        return Err(bad(format!(
                            "`{name}` has no content; give \"\" for an empty file"
                        )))
                    }
                    Some(Value::String { val, .. }) => Content::Bytes(val.clone().into_bytes()),
                    Some(v @ Value::Binary { .. }) => Content::Bytes(binary_bytes(v)),
                    Some(v) => Content::Value(v.clone()),
                }
            };
            let modified = match record.get("modified") {
                None | Some(Value::Nothing { .. }) => None,
                Some(Value::Date { val, .. }) => Some(val.naive_local()),
                Some(v) => {
                    return Err(bad(format!(
                        "`{name}`: `modified` must be a datetime, not {}",
                        v.get_type()
                    )))
                }
            };
            Ok(Item {
                name,
                content,
                modified,
                origin,
            })
        })
        .collect()
}

/// Every `Content::Value` turned into bytes by the engine's `to <ext>`, as `save` would.
pub(crate) fn serialize(
    engine: &EngineInterface,
    items: Vec<Item>,
    span: Span,
) -> Result<Vec<Item>, LabeledError> {
    items
        .into_iter()
        .map(|item| {
            let Content::Value(value) = item.content else {
                return Ok(item);
            };
            let bad =
                |msg: String| LabeledError::new(format!("{}: `{}`: {msg}", item.origin, item.name));
            let kind = value.get_type();
            let Some(ext) = extension(&item.name) else {
                return Err(bad(format!("no extension to pick a format for a {kind}")));
            };
            let Some(decl) = engine.find_decl(format!("to {ext}"))? else {
                return Err(bad(format!(
                    "no `to {ext}` in scope to write a {kind} as .{ext}"
                )));
            };
            let out = engine
                .call_decl(
                    decl,
                    EvaluatedCall::new(span),
                    PipelineData::value(value, None),
                    true,
                    false,
                )
                .map_err(|e| bad(format!("`to {ext}` failed: {e}")))?
                .into_value(span)
                .map_err(|e| bad(format!("`to {ext}` failed: {e}")))?;
            let bytes = match out {
                Value::String { val, .. } => val.into_bytes(),
                v @ Value::Binary { .. } => binary_bytes(&v),
                other => {
                    return Err(bad(format!(
                        "`to {ext}` returned a {}, not a string or binary",
                        other.get_type()
                    )))
                }
            };
            Ok(Item {
                content: Content::Bytes(bytes),
                ..item
            })
        })
        .collect()
}

/// The archive's bytes. Each entry's date is `mtime` when given, else its `modified`,
/// else `now`.
pub(crate) fn build(
    items: &[Item],
    mtime: Option<NaiveDateTime>,
    now: NaiveDateTime,
) -> Result<Vec<u8>, LabeledError> {
    let mut w = Writer::new(Cursor::new(Vec::new()));
    for item in items {
        let date = mtime.or(item.modified).unwrap_or(now);
        let result = match &item.content {
            Content::Dir => w.dir(&item.name, date),
            Content::Bytes(b) => w.file(&item.name, date, b.len() as u64, &mut b.as_slice()),
            Content::Value(_) => {
                return Err(LabeledError::new(format!(
                    "{}: `{}`: content was not serialized",
                    item.origin, item.name
                )))
            }
        };
        result.map_err(|e| LabeledError::new(format!("{}: {e}", item.origin)))?;
    }
    w.finish()
        .map(Cursor::into_inner)
        .map_err(|e| LabeledError::new(e.to_string()))
}

/// The bytes of a binary value. The payload of `Value::Binary` is a `Vec<u8>` on some
/// Nushell minors and a shared wrapper on others, so it is read through the accessor.
fn binary_bytes(value: &Value) -> Vec<u8> {
    value.as_binary().map(<[u8]>::to_vec).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::entries;
    use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
    use nu_protocol::{record, Span, Value};
    use std::io::Cursor;
    use zip::ZipArchive;

    fn s() -> Span {
        Span::test_data()
    }

    fn at(y: i32, mo: u32, d: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .and_then(|d| d.and_hms_opt(12, 0, 0))
            .expect("valid date")
    }

    fn row(name: Value, content: Option<Value>) -> Value {
        let mut r = record! { "name" => name };
        if let Some(c) = content {
            r.push("content", c);
        }
        Value::record(r, s())
    }

    fn file(name: &str, content: &str) -> Value {
        row(Value::string(name, s()), Some(Value::string(content, s())))
    }

    fn message(e: &LabeledError) -> String {
        e.to_string()
    }

    #[test]
    fn rows_become_items() {
        let rows = [
            file("a.txt", "hi"),
            row(
                Value::string("b.bin", s()),
                Some(Value::binary(vec![1, 2], s())),
            ),
            row(Value::string("d/", s()), None),
            row(
                Value::string("l.nuon", s()),
                Some(Value::list(vec![Value::int(1, s())], s())),
            ),
        ];
        let items = from_rows(&rows).expect("items");
        assert_eq!(items.len(), 4);
        assert!(matches!(&items[0].content, Content::Bytes(b) if b == b"hi"));
        assert!(matches!(&items[1].content, Content::Bytes(b) if b == &[1, 2]));
        assert!(matches!(items[2].content, Content::Dir));
        assert!(matches!(
            items[3].content,
            Content::Value(Value::List { .. })
        ));
        assert_eq!(items[0].name, "a.txt");
        assert_eq!(items[2].name, "d/");
        assert_eq!(items[3].origin, "row 3");
        assert!(items.iter().all(|i| i.modified.is_none()));
    }

    #[test]
    fn row_errors_name_the_row() {
        let with = |name: &str, content: Option<Value>, modified: Option<Value>| {
            let mut r = record! { "name" => Value::string(name, s()) };
            if let Some(c) = content {
                r.push("content", c);
            }
            if let Some(m) = modified {
                r.push("modified", m);
            }
            Value::record(r, s())
        };
        let cases = [
            (
                Value::record(record! { "content" => Value::string("x", s()) }, s()),
                "no `name` column",
            ),
            (
                row(Value::int(3, s()), Some(Value::string("x", s()))),
                "`name` must be a string",
            ),
            (with("a.txt", None, None), "has no content"),
            (
                with("a.txt", Some(Value::nothing(s())), None),
                "has no content",
            ),
            (
                with("d/", Some(Value::string("x", s())), None),
                "cannot hold content",
            ),
            (
                with(
                    "a.txt",
                    Some(Value::string("x", s())),
                    Some(Value::int(3, s())),
                ),
                "`modified` must be a datetime",
            ),
            (Value::int(7, s()), "expected a record"),
        ];
        for (bad, why) in cases {
            let rows = [file("ok.txt", "fine"), bad.clone()];
            let e = from_rows(&rows).expect_err("refused");
            let m = message(&e);
            assert!(m.contains("row 1") && m.contains(why), "{bad:?}: {m}");
        }
    }

    #[test]
    fn build_dates() {
        let now = at(2030, 6, 1);
        let given = Utc
            .with_ymd_and_hms(2020, 3, 4, 5, 6, 8)
            .single()
            .expect("date")
            .fixed_offset();
        let mut dated = record! {
            "name" => Value::string("b.txt", s()),
            "content" => Value::string("y", s()),
        };
        dated.push("modified", Value::date(given, s()));
        let rows = [file("a.txt", "x"), Value::record(dated, s())];
        let items = from_rows(&rows).expect("items");

        let read_dates = |mtime| {
            let bytes = build(&items, mtime, now).expect("build");
            let zip = ZipArchive::new(Cursor::new(bytes)).expect("read back");
            entries(&zip)
                .into_iter()
                .map(|e| e.modified.expect("dated"))
                .collect::<Vec<_>>()
        };
        assert_eq!(read_dates(None), [now, given.naive_local()]);
        let fixed = at(2026, 1, 1);
        assert_eq!(read_dates(Some(fixed)), [fixed, fixed]);
    }

    #[test]
    fn build_names_the_origin() {
        let items = from_rows(&[file("a.txt", "1"), file("a.txt", "2")]).expect("items");
        let e = build(&items, None, at(2026, 1, 1)).expect_err("duplicate");
        let m = message(&e);
        assert!(m.contains("row 1") && m.contains("a.txt"), "{m}");
    }

    #[test]
    fn empty_input_is_22_bytes() {
        assert_eq!(build(&[], None, at(2026, 1, 1)).expect("build").len(), 22);
    }
}
