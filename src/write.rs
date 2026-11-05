//! The writer core both archive writers share: name and date rules, and a `Writer` that
//! keeps directories explicit, refuses collisions, and writes the same bytes on every host.
//!
//! Archives are byte-identical across hosts for the same input: every entry says it was made
//! on Unix (the `zip` crate would otherwise record the current host), carries a fixed mode,
//! and takes an explicit date.

use std::collections::HashSet;
use std::fmt;
use std::io::{Read, Seek, Write};

use chrono::{NaiveDate, NaiveDateTime};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, System, ZipWriter};

#[derive(Debug)]
pub enum WriteError {
    InvalidName {
        name: String,
        reason: &'static str,
    },
    Duplicate(String),
    /// One name used as both a file and a directory.
    FileAndDir(String),
    /// A `--mtime` outside 1980 to 2107.
    Date(String),
    Io(String),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name, reason } => write!(f, "`{name}`: {reason}"),
            Self::Duplicate(name) => write!(f, "`{name}`: written twice"),
            Self::FileAndDir(name) => write!(f, "`{name}`: used as both a file and a directory"),
            Self::Date(msg) | Self::Io(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for WriteError {}

impl From<zip::result::ZipError> for WriteError {
    fn from(e: zip::result::ZipError) -> Self {
        Self::Io(e.to_string())
    }
}

/// Checks an entry name and returns it normalized: one leading `./` and a trailing `/` removed.
///
/// # Errors
///
/// When the name is empty, absolute, holds a backslash, a `..` or `.` segment, or an empty
/// segment.
pub fn check_name(name: &str) -> Result<String, WriteError> {
    let bad = |reason| WriteError::InvalidName {
        name: name.to_string(),
        reason,
    };
    if name.is_empty() {
        return Err(bad("is empty"));
    }
    if name.contains('\\') {
        return Err(bad("contains a backslash"));
    }
    let b = name.as_bytes();
    if name.starts_with('/') || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':') {
        return Err(bad("is an absolute path"));
    }
    let trimmed = name.strip_prefix("./").unwrap_or(name);
    let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
    if trimmed.is_empty() {
        return Err(bad("is empty"));
    }
    for segment in trimmed.split('/') {
        match segment {
            "" => return Err(bad("contains an empty segment")),
            "." => return Err(bad("contains a `.` segment")),
            ".." => return Err(bad("contains a `..` segment")),
            _ => {}
        }
    }
    Ok(trimmed.to_string())
}

fn earliest() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(1980, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .unwrap_or_default()
}

fn latest() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2107, 12, 31)
        .and_then(|d| d.and_hms_opt(23, 59, 58))
        .unwrap_or_default()
}

/// Checks that an explicit date fits a zip's range, 1980-01-01 to 2107-12-31.
///
/// # Errors
///
/// When the date is outside that range.
pub fn check_mtime(t: NaiveDateTime) -> Result<NaiveDateTime, WriteError> {
    if t < earliest() || t.date() > latest().date() {
        return Err(WriteError::Date(format!(
            "{t}: a zip holds dates from 1980 to 2107"
        )));
    }
    Ok(t)
}

/// The zip form of a date, clamped into the range a zip holds: never fails.
#[must_use]
pub fn zip_date(t: NaiveDateTime) -> zip::DateTime {
    zip::DateTime::try_from(t.clamp(earliest(), latest())).unwrap_or_default()
}

pub struct Writer<W: Write + Seek> {
    zip: ZipWriter<W>,
    dirs: HashSet<String>,
    files: HashSet<String>,
}

impl<W: Write + Seek> Writer<W> {
    pub fn new(w: W) -> Self {
        Self {
            zip: ZipWriter::new(w),
            dirs: HashSet::new(),
            files: HashSet::new(),
        }
    }

    fn options(date: NaiveDateTime) -> SimpleFileOptions {
        SimpleFileOptions::default()
            .system(System::Unix)
            .last_modified_time(zip_date(date))
    }

    /// Writes `name` (already checked) and the parents not written yet, outermost first.
    fn ensure_dir(&mut self, name: &str, date: NaiveDateTime) -> Result<(), WriteError> {
        let options = Self::options(date)
            .compression_method(CompressionMethod::Stored)
            .unix_permissions(0o755);
        let ends = name
            .match_indices('/')
            .map(|(i, _)| i)
            .chain(std::iter::once(name.len()));
        for end in ends {
            let prefix = &name[..end];
            if self.files.contains(prefix) {
                return Err(WriteError::FileAndDir(prefix.to_string()));
            }
            if self.dirs.insert(prefix.to_string()) {
                self.zip
                    .add_directory(prefix, options)
                    .map_err(|e| WriteError::Io(format!("`{prefix}`: {e}")))?;
            }
        }
        Ok(())
    }

    /// A directory entry `name/`, and its missing parents. Idempotent: a directory already
    /// written is skipped.
    ///
    /// # Errors
    ///
    /// When the name is invalid, or was written as a file.
    pub fn dir(&mut self, name: &str, date: NaiveDateTime) -> Result<(), WriteError> {
        let name = check_name(name)?;
        self.ensure_dir(&name, date)
    }

    /// A file entry; missing parent directories are written first, with `date`.
    /// `size_hint` is the expected size: from the zip64 threshold up, the entry is declared
    /// large so the header can grow.
    ///
    /// # Errors
    ///
    /// When the name is invalid, already written, or collides with a directory; when reading
    /// `content` or writing the archive fails.
    pub fn file(
        &mut self,
        name: &str,
        date: NaiveDateTime,
        size_hint: u64,
        content: &mut dyn Read,
    ) -> Result<(), WriteError> {
        let name = check_name(name)?;
        if self.files.contains(&name) {
            return Err(WriteError::Duplicate(name));
        }
        if self.dirs.contains(&name) {
            return Err(WriteError::FileAndDir(name));
        }
        if let Some((parent, _)) = name.rsplit_once('/') {
            self.ensure_dir(parent, date)?;
        }
        let options = Self::options(date)
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644)
            .large_file(size_hint >= zip::ZIP64_BYTES_THR);
        self.zip
            .start_file(&name, options)
            .map_err(|e| WriteError::Io(format!("`{name}`: {e}")))?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match content.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(WriteError::Io(format!("`{name}`: cannot read: {e}"))),
            };
            self.zip.write_all(&buf[..n]).map_err(|e| {
                WriteError::Io(format!("`{name}`: cannot write to the archive: {e}"))
            })?;
        }
        self.files.insert(name);
        Ok(())
    }

    /// Writes the central directory and returns the underlying writer.
    ///
    /// # Errors
    ///
    /// When the archive cannot be finished.
    pub fn finish(self) -> Result<W, WriteError> {
        Ok(self.zip.finish()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{entries, read_member};
    use chrono::{NaiveDate, NaiveDateTime};
    use std::io::Cursor;
    use zip::{CompressionMethod, ZipArchive};

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .and_then(|d| d.and_hms_opt(h, mi, s))
            .expect("valid date")
    }

    fn t() -> NaiveDateTime {
        at(2026, 1, 1, 0, 0, 0)
    }

    fn new() -> Writer<Cursor<Vec<u8>>> {
        Writer::new(Cursor::new(vec![]))
    }

    fn put(w: &mut Writer<Cursor<Vec<u8>>>, name: &str, bytes: &[u8]) -> Result<(), WriteError> {
        w.file(name, t(), bytes.len() as u64, &mut &bytes[..])
    }

    fn open(w: Writer<Cursor<Vec<u8>>>) -> ZipArchive<Cursor<Vec<u8>>> {
        ZipArchive::new(Cursor::new(w.finish().expect("finish").into_inner())).expect("read back")
    }

    fn names(zip: &ZipArchive<Cursor<Vec<u8>>>) -> Vec<String> {
        zip.file_names().flatten().map(|n| n.to_string()).collect()
    }

    #[test]
    fn empty_archive_is_22_bytes() {
        let bytes = new().finish().expect("finish").into_inner();
        assert_eq!(bytes, [b"PK\x05\x06".as_slice(), &[0; 18]].concat());
    }

    #[test]
    fn check_name_refuses() {
        for bad in [
            "", "/x", "//x", "C:/x", "C:x", "a\\b", "../x", "a/../b", "a//b", "a/./b",
        ] {
            assert!(
                matches!(check_name(bad), Err(WriteError::InvalidName { .. })),
                "{bad:?}"
            );
        }
        assert_eq!(check_name("./a/b").expect("ok"), "a/b");
        assert_eq!(check_name("a/").expect("ok"), "a");
    }

    #[test]
    fn error_names_the_entry() {
        let e = check_name("a/../b").expect_err("refused");
        assert_eq!(e.to_string(), "`a/../b`: contains a `..` segment");
    }

    #[test]
    fn parents_get_explicit_entries() {
        let mut w = new();
        put(&mut w, "a/b/c.txt", b"x").expect("file");
        let zip = open(w);
        assert_eq!(names(&zip), ["a/", "a/b/", "a/b/c.txt"]);
        let all = entries(&zip);
        assert!(all.iter().any(|e| e.name.contains("c.txt")));
    }

    #[test]
    fn directory_written_once() {
        let mut w = new();
        put(&mut w, "a/x", b"1").expect("file");
        w.dir("a", t()).expect("dir");
        w.dir("a/", t()).expect("dir again");
        assert_eq!(names(&open(w)), ["a/", "a/x"]);

        let mut w = new();
        w.dir("a", t()).expect("dir");
        put(&mut w, "a/x", b"1").expect("file");
        assert_eq!(names(&open(w)), ["a/", "a/x"]);
    }

    #[test]
    fn duplicate_and_conflict() {
        let mut w = new();
        put(&mut w, "a", b"1").expect("file");
        assert!(matches!(
            put(&mut w, "a", b"2"),
            Err(WriteError::Duplicate(_))
        ));
        assert!(matches!(
            put(&mut w, "a/b", b"2"),
            Err(WriteError::FileAndDir(_))
        ));

        let mut w = new();
        w.dir("a", t()).expect("dir");
        assert!(matches!(
            put(&mut w, "a", b"2"),
            Err(WriteError::FileAndDir(_))
        ));
    }

    #[test]
    fn same_input_same_bytes() {
        let build = || {
            let mut w = new();
            put(&mut w, "a/b.txt", b"hello").expect("file");
            w.dir("c", t()).expect("dir");
            w.finish().expect("finish").into_inner()
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn dates_clamp_and_check() {
        let lo = zip_date(at(1970, 1, 1, 0, 0, 0));
        assert_eq!(
            (
                lo.year(),
                lo.month(),
                lo.day(),
                lo.hour(),
                lo.minute(),
                lo.second()
            ),
            (1980, 1, 1, 0, 0, 0)
        );
        let hi = zip_date(at(2200, 1, 1, 0, 0, 0));
        assert_eq!(
            (
                hi.year(),
                hi.month(),
                hi.day(),
                hi.hour(),
                hi.minute(),
                hi.second()
            ),
            (2107, 12, 31, 23, 59, 58)
        );
        assert!(matches!(
            check_mtime(at(1979, 12, 31, 23, 59, 59)),
            Err(WriteError::Date(_))
        ));
        assert!(matches!(
            check_mtime(at(2108, 1, 1, 0, 0, 0)),
            Err(WriteError::Date(_))
        ));
        assert_eq!(
            check_mtime(at(2026, 1, 1, 0, 0, 0)).expect("ok"),
            at(2026, 1, 1, 0, 0, 0)
        );
    }

    #[test]
    fn non_ascii_round_trips() {
        let mut w = new();
        put(&mut w, "données/é.txt", b"x").expect("file");
        let mut zip = open(w);
        assert_eq!(read_member(&mut zip, "données/é.txt").expect("read"), b"x");
    }

    #[test]
    fn content_round_trips_deflated() {
        let data = b"abcdefghij".repeat(1024);
        let mut w = new();
        put(&mut w, "d/f.bin", &data).expect("file");
        let mut zip = open(w);
        assert_eq!(read_member(&mut zip, "d/f.bin").expect("read"), data);

        let file = zip.by_name("d/f.bin").expect("file");
        assert_eq!(file.compression(), CompressionMethod::Deflated);
        assert_eq!(file.unix_mode(), Some(0o100_644));
        drop(file);
        let dir = zip.by_name("d/").expect("dir");
        assert_eq!(dir.compression(), CompressionMethod::Stored);
        assert_eq!(dir.unix_mode().map(|m| m & 0o777), Some(0o755));
    }

    fn golden() -> Vec<u8> {
        let mut w = new();
        put(&mut w, "readme.txt", b"hello\n").expect("file");
        put(&mut w, "data/a.json", b"{\"x\": 1}").expect("file");
        w.finish().expect("finish").into_inner()
    }

    #[test]
    fn golden_bytes_are_stable() {
        assert!(
            golden() == include_bytes!("../tests/fixtures/golden.zip"),
            "the zip crate wrote different bytes"
        );
    }

    struct Failing;
    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("boom"))
        }
    }

    #[test]
    fn read_failure_names_the_entry() {
        let mut w = new();
        let e = w
            .file("a/b.txt", t(), 0, &mut Failing)
            .expect_err("read fails");
        assert!(matches!(e, WriteError::Io(_)));
        assert_eq!(e.to_string(), "`a/b.txt`: cannot read: boom");
    }
}
