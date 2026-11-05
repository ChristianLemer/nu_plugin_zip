use std::io::Cursor;
use std::path::Path;

use nu_plugin::{EngineInterface, EvaluatedCall, SimplePluginCommand};
use nu_protocol::{
    record, ByteStream, ByteStreamType, Category, Example, LabeledError, PipelineData, Signals,
    Signature, Span, Spanned, SyntaxShape, Type, Value,
};

use crate::archive::read_member;
use crate::{resolve, ZipPlugin};

pub struct ZipOpen;

#[allow(clippy::unnecessary_literal_bound)]
impl SimplePluginCommand for ZipOpen {
    type Plugin = ZipPlugin;

    fn name(&self) -> &str {
        "zip open"
    }

    fn description(&self) -> &str {
        "Read members of a zip archive, parsed by their extension like open"
    }

    fn extra_description(&self) -> &str {
        "A member named data/a.json goes through `from json`, so any `from` command in \
         scope applies. Without one, the bytes come back as a string when they are UTF-8, \
         binary otherwise. Several paths give a table of name and content, read in one \
         call. Members are decompressed in memory; nothing is written to disk."
    }

    fn signature(&self) -> Signature {
        Signature::build("zip open")
            .input_output_type(Type::Nothing, Type::Any)
            .required("archive", SyntaxShape::Filepath, "The zip archive to read")
            .rest(
                "paths",
                SyntaxShape::String,
                "Members to read, relative to the archive root",
            )
            .switch("raw", "Return the bytes without parsing them", Some('r'))
            .category(Category::FileSystem)
    }

    fn search_terms(&self) -> Vec<&str> {
        vec!["zip", "archive", "read", "extract", "unzip"]
    }

    fn examples(&self) -> Vec<Example<'_>> {
        vec![
            Example {
                description: "Read a JSON member as a record",
                example: "zip open archive.zip data/a.json",
                result: None,
            },
            Example {
                description: "Read a member as raw bytes",
                example: "zip open --raw archive.zip bin/blob.bin",
                result: None,
            },
            Example {
                description: "Read several members in one call",
                example: "zip open archive.zip data/a.json data/b.nuon",
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
        let paths: Vec<Spanned<String>> = call.rest(1)?;
        let raw = call.has_flag("raw")?;
        if paths.is_empty() {
            return Err(LabeledError::new("no member to open")
                .with_label("give at least one path inside the archive", span));
        }
        let path = resolve(engine, &archive.item)
            .map_err(|e| e.with_label("cannot open this archive", archive.span))?;
        // Read under the cache lock, parse after it: parsing calls back into the engine,
        // which must not wait on a lock this plugin holds.
        let members = plugin
            .with_archive(engine, &path, |c| {
                paths
                    .iter()
                    .map(|p| {
                        read_member(&mut c.zip, &p.item).map_err(|e| {
                            LabeledError::new(e.to_string())
                                .with_label("cannot read this member", p.span)
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|e| e.with_label("cannot read this archive", archive.span))??;
        let mut values = paths
            .iter()
            .zip(members)
            .map(|(p, bytes)| {
                if raw {
                    Ok(Value::binary(bytes, span))
                } else {
                    parse(engine, &p.item, bytes, span)
                }
            })
            .collect::<Result<Vec<_>, LabeledError>>()?;
        if values.len() == 1 {
            return Ok(values.remove(0));
        }
        let rows = paths
            .iter()
            .zip(values)
            .map(|(p, content)| {
                Value::record(
                    record! {
                        "name" => Value::string(&p.item, span),
                        "content" => content,
                    },
                    span,
                )
            })
            .collect();
        Ok(Value::list(rows, span))
    }
}

/// The member's bytes through the engine's `from <ext>`, as `open` would. They go as a
/// byte stream of unknown type, as `open` sends a file: a binary value would be refused
/// by the parsers that take text, such as `from json`.
fn parse(
    engine: &EngineInterface,
    name: &str,
    bytes: Vec<u8>,
    span: Span,
) -> Result<Value, LabeledError> {
    let Some(ext) = extension(name) else {
        return Ok(unparsed(bytes, span));
    };
    let Some(decl) = engine.find_decl(format!("from {ext}"))? else {
        return Ok(unparsed(bytes, span));
    };
    let parsed = engine.call_decl(
        decl,
        EvaluatedCall::new(span),
        PipelineData::byte_stream(
            ByteStream::read(
                Cursor::new(bytes),
                span,
                Signals::empty(),
                ByteStreamType::Unknown,
            ),
            None,
        ),
        true,
        false,
    )?;
    Ok(parsed.into_value(span)?)
}

/// The lowercased extension of the member's file name, if it has one.
pub(crate) fn extension(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
}

/// What `open` returns for a format it has no parser for.
fn unparsed(bytes: Vec<u8>, span: Span) -> Value {
    match String::from_utf8(bytes) {
        Ok(s) => Value::string(s, span),
        Err(e) => Value::binary(e.into_bytes(), span),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_is_lowercased_and_file_only() {
        assert_eq!(extension("data/A.JSON").as_deref(), Some("json"));
        assert_eq!(extension("Makefile"), None);
        assert_eq!(extension("dir.d/file"), None);
    }

    #[test]
    fn unparsed_is_string_when_utf8() {
        assert_eq!(
            unparsed(b"hi".to_vec(), Span::test_data()).as_str().ok(),
            Some("hi")
        );
        assert!(unparsed(vec![0xff, 0xfe], Span::test_data())
            .as_binary()
            .is_ok());
    }
}
