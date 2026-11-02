use chrono::{Local, NaiveDateTime, TimeZone};
use nu_plugin::{EngineInterface, EvaluatedCall, SimplePluginCommand};
use nu_protocol::{
    record, Category, Example, LabeledError, Signature, Span, Spanned, SyntaxShape, Type, Value,
};

use crate::archive::{children, normalize, Entry};
use crate::{resolve, ZipPlugin};

pub struct ZipLs;

#[allow(clippy::unnecessary_literal_bound)]
impl SimplePluginCommand for ZipLs {
    type Plugin = ZipPlugin;

    fn name(&self) -> &str {
        "zip ls"
    }

    fn description(&self) -> &str {
        "List the contents of a directory inside a zip archive, like ls"
    }

    fn extra_description(&self) -> &str {
        "Names are full paths inside the archive. Directories the archive only implies \
         (a/b.txt with no a/ entry) are listed as dir all the same. Nothing is extracted."
    }

    fn signature(&self) -> Signature {
        Signature::build("zip ls")
            .input_output_type(Type::Nothing, Type::table())
            .required("archive", SyntaxShape::Filepath, "The zip archive to read")
            .optional(
                "dir",
                SyntaxShape::String,
                "Directory inside the archive, relative to its root (default: the root)",
            )
            .category(Category::FileSystem)
    }

    fn search_terms(&self) -> Vec<&str> {
        vec!["zip", "archive", "list", "browse"]
    }

    fn examples(&self) -> Vec<Example<'_>> {
        vec![
            Example {
                description: "List the root of an archive",
                example: "zip ls archive.zip",
                result: None,
            },
            Example {
                description: "List one directory inside it",
                example: "zip ls archive.zip data",
                result: None,
            },
        ]
    }

    fn run(
        &self,
        plugin: &ZipPlugin,
        engine: &EngineInterface,
        call: &EvaluatedCall,
        _input: &Value,
    ) -> Result<Value, LabeledError> {
        let span = call.head;
        let archive: Spanned<String> = call.req(0)?;
        let dir: Option<Spanned<String>> = call.opt(1)?;
        let path = resolve(engine, &archive.item)
            .map_err(|e| e.with_label("cannot open this archive", archive.span))?;
        plugin
            .with_archive(engine, &path, |c| {
                let wanted = dir.as_ref().map_or("", |d| d.item.as_str());
                match listing(&c.entries, wanted) {
                    Some(found) => Ok(rows(&found, span)),
                    None => Err(LabeledError::new(format!(
                        "no such directory in archive: {wanted}"
                    ))
                    .with_label(
                        "not a directory in this archive",
                        dir.as_ref().map_or(span, |d| d.span),
                    )),
                }
            })
            .map_err(|e| e.with_label("cannot read this archive", archive.span))?
    }
}

/// The direct children of `dir`, or `None` when the archive holds no such directory.
fn listing<'a>(entries: &'a [Entry], dir: &str) -> Option<Vec<&'a Entry>> {
    let wanted = normalize(dir);
    let exists = wanted.is_empty() || entries.iter().any(|e| e.is_dir && e.name == wanted);
    exists.then(|| children(entries, &wanted))
}

/// One `ls`-shaped row per entry.
pub(crate) fn rows(entries: &[&Entry], span: Span) -> Value {
    Value::list(
        entries
            .iter()
            .map(|e| {
                Value::record(
                    record! {
                        "name" => Value::string(&e.name, span),
                        "type" => Value::string(if e.is_dir { "dir" } else { "file" }, span),
                        "size" => filesize(e.size, span),
                        "modified" => modified(e.modified, span),
                    },
                    span,
                )
            })
            .collect(),
        span,
    )
}

pub(crate) fn filesize(size: u64, span: Span) -> Value {
    Value::filesize(i64::try_from(size).unwrap_or(i64::MAX), span)
}

/// A zip timestamp carries no zone; the tools that write it use local time, and `ls`
/// shows local times, so it is read as the host's local time. A time that falls in a
/// daylight-saving gap does not exist locally and is read as UTC instead.
pub(crate) fn modified(naive: Option<NaiveDateTime>, span: Span) -> Value {
    match naive {
        Some(n) => {
            let local = Local
                .from_local_datetime(&n)
                .earliest()
                .map_or_else(|| n.and_utc().fixed_offset(), |d| d.fixed_offset());
            Value::date(local, span)
        }
        None => Value::nothing(span),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool, size: u64, modified: bool) -> Entry {
        Entry {
            name: name.into(),
            is_dir,
            size,
            modified: modified.then(|| {
                chrono::NaiveDate::from_ymd_opt(2024, 1, 2)
                    .and_then(|d| d.and_hms_opt(3, 4, 5))
                    .expect("valid date")
            }),
        }
    }

    #[test]
    fn rows_match_ls_columns() {
        let dir = entry("data", true, 0, false);
        let file = entry("data/a.json", false, 8, true);
        let table = rows(&[&dir, &file], Span::test_data());
        let rows = table.as_list().expect("list");
        let d = rows[0].as_record().expect("record");
        assert_eq!(
            d.columns().collect::<Vec<_>>(),
            ["name", "type", "size", "modified"]
        );
        assert_eq!(d.get("name").and_then(|v| v.as_str().ok()), Some("data"));
        assert_eq!(d.get("type").and_then(|v| v.as_str().ok()), Some("dir"));
        assert_eq!(
            d.get("size").and_then(|v| v.as_filesize().ok()),
            Some(0.into())
        );
        assert!(d.get("modified").is_some_and(Value::is_nothing));
        let f = rows[1].as_record().expect("record");
        assert_eq!(f.get("type").and_then(|v| v.as_str().ok()), Some("file"));
        assert_eq!(
            f.get("size").and_then(|v| v.as_filesize().ok()),
            Some(8.into())
        );
        let modified = f
            .get("modified")
            .and_then(|v| v.as_date().ok())
            .expect("date");
        assert_eq!(
            modified.naive_local(),
            entry("", false, 0, true).modified.expect("date")
        );
    }

    #[test]
    fn listing_refuses_what_is_not_a_directory() {
        let all = [
            entry("data", true, 0, false),
            entry("data/a.json", false, 8, true),
        ];
        assert_eq!(listing(&all, "").map(|l| l.len()), Some(1));
        assert_eq!(listing(&all, "./data/").map(|l| l.len()), Some(1));
        assert!(listing(&all, "data/a.json").is_none());
        assert!(listing(&all, "nope").is_none());
    }
}
