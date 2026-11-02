use globset::GlobBuilder;
use nu_plugin::{EngineInterface, EvaluatedCall, SimplePluginCommand};
use nu_protocol::{Category, Example, LabeledError, Signature, Spanned, SyntaxShape, Type, Value};

use crate::archive::{normalize, Entry};
use crate::{resolve, ZipPlugin};

pub struct ZipGlob;

#[allow(clippy::unnecessary_literal_bound)]
impl SimplePluginCommand for ZipGlob {
    type Plugin = ZipPlugin;

    fn name(&self) -> &str {
        "zip glob"
    }

    fn description(&self) -> &str {
        "List the paths inside a zip archive that match a glob pattern, like glob"
    }

    fn extra_description(&self) -> &str {
        "* stays within one path segment, ** crosses them, and matching is case-sensitive. \
         Paths use / whatever the host. Directories match as well as files."
    }

    fn signature(&self) -> Signature {
        Signature::build("zip glob")
            .input_output_type(Type::Nothing, Type::List(Box::new(Type::String)))
            .required("archive", SyntaxShape::Filepath, "The zip archive to read")
            .required(
                "pattern",
                SyntaxShape::String,
                "Glob pattern over paths inside the archive",
            )
            .category(Category::FileSystem)
    }

    fn search_terms(&self) -> Vec<&str> {
        vec!["zip", "archive", "find", "pattern"]
    }

    fn examples(&self) -> Vec<Example<'_>> {
        vec![
            Example {
                description: "Every CSV file at any depth under data",
                example: "zip glob archive.zip 'data/**/*.csv'",
                result: None,
            },
            Example {
                description: "The direct children of data",
                example: "zip glob archive.zip 'data/*'",
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
        let pattern: Spanned<String> = call.req(1)?;
        let path = resolve(engine, &archive.item)
            .map_err(|e| e.with_label("cannot open this archive", archive.span))?;
        let found = plugin
            .with_archive(engine, &path, |c| matching(&c.entries, &pattern.item))
            .map_err(|e| e.with_label("cannot read this archive", archive.span))?
            .map_err(|e| {
                LabeledError::new(format!("invalid glob pattern: {e}"))
                    .with_label("invalid pattern", pattern.span)
            })?;
        Ok(Value::list(
            found.into_iter().map(|n| Value::string(n, span)).collect(),
            span,
        ))
    }
}

/// The names of every entry, file or directory, that `pattern` matches, sorted.
fn matching(entries: &[Entry], pattern: &str) -> Result<Vec<String>, globset::Error> {
    let glob = GlobBuilder::new(&normalize(pattern))
        .literal_separator(true)
        .build()?
        .compile_matcher();
    Ok(entries
        .iter()
        .filter(|e| glob.is_match(&e.name))
        .map(|e| e.name.clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Entry> {
        [
            ("bin", true),
            ("bin/blob.bin", false),
            ("data", true),
            ("data/a.json", false),
            ("data/b.nuon", false),
            ("data/deep", true),
            ("data/deep/c.csv", false),
            ("readme.txt", false),
        ]
        .iter()
        .map(|(name, is_dir)| Entry {
            name: (*name).into(),
            is_dir: *is_dir,
            size: 0,
            modified: None,
        })
        .collect()
    }

    #[test]
    fn star_stays_in_one_segment() {
        assert_eq!(
            matching(&sample(), "data/*").expect("pattern"),
            ["data/a.json", "data/b.nuon", "data/deep"]
        );
        assert_eq!(
            matching(&sample(), "*.txt").expect("pattern"),
            ["readme.txt"]
        );
    }

    #[test]
    fn double_star_crosses_segments() {
        assert_eq!(
            matching(&sample(), "data/**/*.csv").expect("pattern"),
            ["data/deep/c.csv"]
        );
    }

    #[test]
    fn leading_dot_slash_is_accepted() {
        assert_eq!(
            matching(&sample(), "./*.txt").expect("pattern"),
            ["readme.txt"]
        );
    }

    #[test]
    fn matching_is_case_sensitive() {
        assert!(matching(&sample(), "*.TXT").expect("pattern").is_empty());
    }

    #[test]
    fn invalid_pattern_is_an_error() {
        assert!(matching(&sample(), "[").is_err());
    }
}
