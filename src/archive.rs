//! The zip itself, independent of Nushell: what an archive holds, and the bytes of one
//! member. Everything here works on any `Read + Seek`, so the tests run on zips built in
//! memory and the plugin hands it a file.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Seek};

use zip::{CompressionMethod, ZipArchive};

/// One path inside an archive, as `zip ls` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Full path inside the archive, `/`-separated, without a trailing `/`.
    pub name: String,
    pub is_dir: bool,
    /// Uncompressed size; 0 for a directory.
    pub size: u64,
    /// The zip's timestamp, which carries no zone. `None` for an implied directory.
    pub modified: Option<chrono::NaiveDateTime>,
}

#[derive(Debug)]
pub enum ArchiveError {
    NotFound(String),
    IsDir(String),
    Unsupported { member: String, method: String },
    Encrypted(String),
    Zip(String),
    Io(String),
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(m) => write!(f, "no member `{m}` in the archive"),
            Self::IsDir(m) => write!(f, "`{m}` is a directory in the archive, not a file"),
            Self::Unsupported { member, method } => write!(
                f,
                "`{member}` is compressed with {method}, which this plugin does not decode \
                 (stored, deflate and deflate64 only)"
            ),
            Self::Encrypted(m) => write!(f, "`{m}` is encrypted, which this plugin does not read"),
            Self::Zip(e) => write!(f, "not a readable zip: {e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<zip::result::ZipError> for ArchiveError {
    fn from(e: zip::result::ZipError) -> Self {
        Self::Zip(e.to_string())
    }
}

/// A member path as a user types it: no leading `./` or `/`, no trailing `/`.
#[must_use]
pub fn normalize(path: &str) -> String {
    let p = path.strip_prefix("./").unwrap_or(path);
    p.trim_start_matches('/').trim_end_matches('/').to_string()
}

/// Every entry of the archive, plus the directories its member names only imply, sorted
/// by name. Names are kept as stored: nothing here resolves them against a host path.
pub fn entries<R: Read + Seek>(zip: &ZipArchive<R>) -> Vec<Entry> {
    let mut all: BTreeMap<String, Entry> = BTreeMap::new();
    for i in 0..zip.len() {
        let Ok(data) = zip.by_index_data(i) else {
            continue;
        };
        let stored = data.name().map_or_else(
            |_| String::from_utf8_lossy(data.name_raw()).into_owned(),
            std::borrow::Cow::into_owned,
        );
        let is_dir = stored.ends_with('/');
        let name = stored.trim_end_matches('/').to_string();
        if name.is_empty() {
            continue;
        }
        let modified = data
            .last_modified()
            .and_then(|d| chrono::NaiveDateTime::try_from(d).ok());
        add_implied_dirs(&mut all, &name);
        all.insert(
            name.clone(),
            Entry {
                name,
                is_dir,
                size: if is_dir { 0 } else { data.size() },
                modified,
            },
        );
    }
    all.into_values().collect()
}

fn add_implied_dirs(all: &mut BTreeMap<String, Entry>, name: &str) {
    for (i, _) in name.match_indices('/') {
        let dir = &name[..i];
        if !dir.is_empty() && !all.contains_key(dir) {
            all.insert(
                dir.to_string(),
                Entry {
                    name: dir.to_string(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                },
            );
        }
    }
}

/// The direct children of `dir` (`""` for the root), like one level of `ls`.
#[must_use]
pub fn children<'a>(entries: &'a [Entry], dir: &str) -> Vec<&'a Entry> {
    let dir = normalize(dir);
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    entries
        .iter()
        .filter(|e| {
            e.name
                .strip_prefix(&prefix)
                .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
        })
        .collect()
}

/// The bytes of one member, decompressed in memory. The name is tried exactly as given
/// first, so a stored name such as `/etc/x` stays reachable, then normalized.
///
/// # Errors
///
/// When the member is missing, is a directory, is encrypted, uses a compression method
/// other than stored, deflate or deflate64, or fails to decompress.
pub fn read_member<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
) -> Result<Vec<u8>, ArchiveError> {
    let wanted = normalize(name);
    let Some(index) = zip
        .index_for_name(name)
        .or_else(|| zip.index_for_name(&wanted))
    else {
        let as_dir = format!("{wanted}/");
        let is_dir = zip.file_names().flatten().any(|n| n.starts_with(&as_dir));
        return Err(if is_dir && !wanted.is_empty() {
            ArchiveError::IsDir(wanted)
        } else {
            ArchiveError::NotFound(wanted)
        });
    };
    {
        let data = zip.by_index_data(index)?;
        if data.is_dir() {
            return Err(ArchiveError::IsDir(wanted));
        }
        if data.encrypted() {
            return Err(ArchiveError::Encrypted(wanted));
        }
        let method = data.compression();
        if !matches!(
            method,
            CompressionMethod::Stored | CompressionMethod::Deflated | CompressionMethod::Deflate64
        ) {
            return Err(ArchiveError::Unsupported {
                member: wanted,
                method: method.to_string(),
            });
        }
    }
    let mut file = zip.by_index(index)?;
    let mut bytes = Vec::with_capacity(usize::try_from(file.size()).unwrap_or(0));
    file.read_to_end(&mut bytes)
        .map_err(|e| ArchiveError::Zip(format!("`{wanted}`: {e}")))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipArchive, ZipWriter};

    /// A zip written in memory, one member per `(name, method, bytes)`.
    fn build(members: &[(&str, CompressionMethod, &[u8])]) -> Vec<u8> {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, method, data) in members {
            w.start_file(
                *name,
                SimpleFileOptions::default().compression_method(*method),
            )
            .expect("start member");
            w.write_all(data).expect("write member");
        }
        w.finish().expect("finish zip").into_inner()
    }

    fn sample() -> ZipArchive<Cursor<Vec<u8>>> {
        let bytes = build(&[
            ("readme.txt", CompressionMethod::Stored, b"hello\n"),
            ("data/a.json", CompressionMethod::Deflated, b"{\"x\": 1}"),
            ("data/b.nuon", CompressionMethod::Stored, b"{y: 2}"),
            (
                "data/deep/c.csv",
                CompressionMethod::Deflated,
                b"k,v\n1,2\n",
            ),
            ("bin/blob.bin", CompressionMethod::Deflated, &[7; 256]),
        ]);
        ZipArchive::new(Cursor::new(bytes)).expect("valid zip")
    }

    fn names(entries: &[&Entry]) -> Vec<String> {
        entries.iter().map(|e| e.name.clone()).collect()
    }

    /// Overwrite one little-endian u16 field of the first member, in its local header
    /// and in its central directory record, to forge what `ZipWriter` refuses to write.
    fn patch_first_member(
        bytes: &mut [u8],
        local_offset: usize,
        central_offset: usize,
        value: u16,
    ) {
        let cd = bytes
            .windows(4)
            .position(|w| w == b"PK\x01\x02")
            .expect("central directory");
        bytes[local_offset..local_offset + 2].copy_from_slice(&value.to_le_bytes());
        bytes[cd + central_offset..cd + central_offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn implied_dirs_are_listed() {
        let all = entries(&sample());
        let root = children(&all, "");
        assert_eq!(names(&root), ["bin", "data", "readme.txt"]);
        assert!(root[0].is_dir && root[1].is_dir && !root[2].is_dir);
    }

    #[test]
    fn children_of_nested_dir() {
        let all = entries(&sample());
        assert_eq!(
            names(&children(&all, "data")),
            ["data/a.json", "data/b.nuon", "data/deep"]
        );
    }

    #[test]
    fn entries_carry_size_and_date() {
        let all = entries(&sample());
        let csv = all
            .iter()
            .find(|e| e.name == "data/deep/c.csv")
            .expect("csv");
        assert_eq!(csv.size, 8);
        assert!(csv.modified.is_some());
        let dir = all.iter().find(|e| e.name == "data/deep").expect("dir");
        assert_eq!(dir.size, 0);
    }

    #[test]
    fn read_member_round_trips() {
        let mut z = sample();
        assert_eq!(
            read_member(&mut z, "data/a.json").expect("read"),
            b"{\"x\": 1}"
        );
        assert_eq!(
            read_member(&mut z, "./readme.txt").expect("read"),
            b"hello\n"
        );
    }

    #[test]
    fn read_missing_is_not_found() {
        let err = read_member(&mut sample(), "nope.txt").expect_err("missing");
        assert!(matches!(err, ArchiveError::NotFound(ref n) if n == "nope.txt"));
    }

    #[test]
    fn read_dir_is_is_dir() {
        let err = read_member(&mut sample(), "data").expect_err("dir");
        assert!(matches!(err, ArchiveError::IsDir(ref n) if n == "data"));
    }

    #[test]
    fn normalize_strips() {
        assert_eq!(normalize("./a/b/"), "a/b");
        assert_eq!(normalize("/a"), "a");
        assert_eq!(normalize("a"), "a");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn zip_slip_name_is_kept_verbatim() {
        let bytes = build(&[("../evil.txt", CompressionMethod::Stored, b"x")]);
        let mut z = ZipArchive::new(Cursor::new(bytes)).expect("zip");
        assert_eq!(
            names(&entries(&z).iter().collect::<Vec<_>>()),
            ["..", "../evil.txt"]
        );
        assert_eq!(read_member(&mut z, "../evil.txt").expect("read"), b"x");
    }

    #[test]
    fn unsupported_method_names_member() {
        let mut bytes = build(&[("z.bin", CompressionMethod::Stored, b"abc")]);
        // Compression method: offset 8 in the local header, 10 in the central record. 93 = zstd.
        patch_first_member(&mut bytes, 8, 10, 93);
        let mut z = ZipArchive::new(Cursor::new(bytes)).expect("zip");
        let err = read_member(&mut z, "z.bin").expect_err("unsupported");
        assert!(
            matches!(err, ArchiveError::Unsupported { ref member, .. } if member == "z.bin"),
            "{err}"
        );
        assert!(err.to_string().contains("z.bin"));
    }

    #[test]
    fn encrypted_member_is_refused() {
        let mut bytes = build(&[("s.txt", CompressionMethod::Stored, b"abc")]);
        // General purpose flags: offset 6 in the local header, 8 in the central record.
        patch_first_member(&mut bytes, 6, 8, 1);
        let mut z = ZipArchive::new(Cursor::new(bytes)).expect("zip");
        let err = read_member(&mut z, "s.txt").expect_err("encrypted");
        assert!(
            matches!(err, ArchiveError::Encrypted(ref n) if n == "s.txt"),
            "{err}"
        );
    }
}
