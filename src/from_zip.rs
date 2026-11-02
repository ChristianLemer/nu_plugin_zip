use std::io::Cursor;

use nu_plugin::{EngineInterface, EvaluatedCall, SimplePluginCommand};
use nu_protocol::{record, Category, Example, LabeledError, Signature, Span, Type, Value};
use zip::ZipArchive;

use crate::archive::{entries, read_member, ArchiveError};
use crate::zip_ls::{filesize, modified};
use crate::ZipPlugin;

pub struct FromZip;

#[allow(clippy::unnecessary_literal_bound)]
impl SimplePluginCommand for FromZip {
    type Plugin = ZipPlugin;

    fn name(&self) -> &str {
        "from zip"
    }

    fn description(&self) -> &str {
        "Read zip bytes into a table of its file members, with their content"
    }

    fn extra_description(&self) -> &str {
        "This is what `open archive.zip` calls. Every member is decompressed in memory; \
         to read only some of them, `zip open` reads just those."
    }

    fn signature(&self) -> Signature {
        Signature::build("from zip")
            .input_output_type(Type::Binary, Type::table())
            .category(Category::Formats)
    }

    fn search_terms(&self) -> Vec<&str> {
        vec!["zip", "archive", "unzip"]
    }

    fn examples(&self) -> Vec<Example<'_>> {
        vec![Example {
            description: "Every file member of an archive, with its content as binary",
            example: "open archive.zip",
            result: None,
        }]
    }

    fn run(
        &self,
        _plugin: &ZipPlugin,
        _engine: &EngineInterface,
        call: &EvaluatedCall,
        input: &Value,
    ) -> Result<Value, LabeledError> {
        let bytes = input.as_binary()?;
        members(bytes, call.head).map_err(|e| {
            LabeledError::new(e.to_string())
                .with_label("cannot read these bytes as a zip", input.span())
        })
    }
}

/// One row per file member, its content decompressed. The bytes are already in memory,
/// so there is nothing for the cache to save.
fn members(bytes: &[u8], span: Span) -> Result<Value, ArchiveError> {
    let mut zip = ZipArchive::new(Cursor::new(bytes))?;
    let files: Vec<_> = entries(&zip).into_iter().filter(|e| !e.is_dir).collect();
    let mut rows = Vec::with_capacity(files.len());
    for e in files {
        let content = read_member(&mut zip, &e.name)?;
        rows.push(Value::record(
            record! {
                "name" => Value::string(&e.name, span),
                "type" => Value::string("file", span),
                "size" => filesize(e.size, span),
                "modified" => modified(e.modified, span),
                "content" => Value::binary(content, span),
            },
            span,
        ));
    }
    Ok(Value::list(rows, span))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_yields_one_row_per_file() {
        let bytes = include_bytes!("../tests/fixtures/sample.zip");
        let table = members(bytes, Span::test_data()).expect("readable zip");
        let rows = table.as_list().expect("list");
        assert_eq!(rows.len(), 5);
        let readme = rows
            .iter()
            .filter_map(|r| r.as_record().ok())
            .find(|r| r.get("name").and_then(|v| v.as_str().ok()) == Some("readme.txt"))
            .expect("readme row");
        assert_eq!(
            readme.columns().collect::<Vec<_>>(),
            ["name", "type", "size", "modified", "content"]
        );
        assert_eq!(
            readme.get("content").and_then(|v| v.as_binary().ok()),
            Some(&b"hello\n"[..])
        );
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(members(b"not a zip", Span::test_data()).is_err());
    }
}
