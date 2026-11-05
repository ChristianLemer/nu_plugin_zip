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
         Paths use / whatever the host. Directories match too, unless --no-dir."
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
            .switch(
                "no-dir",
                "Leave directories out, implied ones included, like glob --no-dir",
                Some('D'),
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
                description: "Every file, directories left out",
                example: "zip glob archive.zip '**/*' --no-dir",
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
        let no_dir = call.has_flag("no-dir")?;
        let path = resolve(engine, &archive.item)
            .map_err(|e| e.with_label("cannot open this archive", archive.span))?;
        let found = plugin
            .with_archive(engine, &path, |c| {
                matching(&c.entries, &pattern.item, no_dir)
            })
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

/// The names of every entry that `pattern` matches, sorted; directories only when
/// `no_dir` is false. The pattern is tried as typed and normalized, so `/etc/*` reaches a
/// stored absolute name while `./*.txt` still means the root.
fn matching(entries: &[Entry], pattern: &str, no_dir: bool) -> Result<Vec<String>, globset::Error> {
    let compile = |p: &str| {
        GlobBuilder::new(p)
            .literal_separator(true)
            .build()
            .map(|g| g.compile_matcher())
    };
    let raw = compile(pattern)?;
    let normalized = compile(&normalize(pattern))?;
    Ok(entries
        .iter()
        .filter(|e| !(no_dir && e.is_dir))
        .filter(|e| raw.is_match(&e.name) || normalized.is_match(&e.name))
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
    fn no_dir_leaves_directories_out() {
        assert_eq!(
            matching(&sample(), "**/*", true).expect("pattern"),
            [
                "bin/blob.bin",
                "data/a.json",
                "data/b.nuon",
                "data/deep/c.csv",
                "readme.txt"
            ]
        );
        assert!(matching(&sample(), "**/*", false)
            .expect("pattern")
            .contains(&"data/deep".to_string()));
    }

    #[test]
    fn star_stays_in_one_segment() {
        assert_eq!(
            matching(&sample(), "data/*", false).expect("pattern"),
            ["data/a.json", "data/b.nuon", "data/deep"]
        );
        assert_eq!(
            matching(&sample(), "*.txt", false).expect("pattern"),
            ["readme.txt"]
        );
    }

    #[test]
    fn double_star_crosses_segments() {
        assert_eq!(
            matching(&sample(), "data/**/*.csv", false).expect("pattern"),
            ["data/deep/c.csv"]
        );
    }

    #[test]
    fn leading_dot_slash_is_accepted() {
        assert_eq!(
            matching(&sample(), "./*.txt", false).expect("pattern"),
            ["readme.txt"]
        );
    }

    #[test]
    fn matching_is_case_sensitive() {
        assert_eq!(
            matching(&sample(), "*.TXT", false).expect("pattern"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn invalid_pattern_is_an_error() {
        assert!(matching(&sample(), "[", false).is_err());
    }

    #[test]
    fn absolute_pattern_reaches_stored_absolute_names() {
        let mut all = sample();
        for (name, is_dir) in [("/etc", true), ("/etc/x", false)] {
            all.push(Entry {
                name: name.into(),
                is_dir,
                size: 0,
                modified: None,
            });
        }
        assert_eq!(
            matching(&all, "/etc/*", false).expect("pattern"),
            ["/etc/x"]
        );
        assert_eq!(
            matching(&all, "/*.txt", false).expect("pattern"),
            ["readme.txt"]
        );
    }
}
