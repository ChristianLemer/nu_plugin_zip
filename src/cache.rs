//! Opened archives kept between calls, so reading N members costs one index read, not N.
//! Each call re-stats the path: a file replaced in place, as a sync client does, is
//! reopened rather than served from a stale index.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use zip::ZipArchive;

use crate::archive::{entries, ArchiveError, Entry};

/// One opened archive and its listing, computed once at open.
pub struct Cached {
    pub zip: ZipArchive<BufReader<File>>,
    pub entries: Vec<Entry>,
}

/// What tells a file apart from the one that was opened. Size and mtime alone miss a
/// replacement that keeps both, as a sync client or `rsync -t` does. On Unix the inode
/// and the ctime close that gap: a rename brings another inode, a rewrite in place moves
/// the ctime, and no tool can set the ctime back. Windows offers nothing comparable in
/// stable Rust, so there the check stays at size and mtime.
#[derive(PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> std::io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            size: meta.len(),
            modified: meta.modified()?,
            #[cfg(unix)]
            identity: (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
        })
    }
}

struct Slot {
    path: PathBuf,
    stamp: Stamp,
    cached: Cached,
}

/// At most `capacity` archives, most recently used first. Small enough that a `Vec`
/// moved-to-front beats any map.
pub struct ArchiveCache {
    capacity: usize,
    slots: Vec<Slot>,
    #[cfg(test)]
    opens: usize,
}

impl ArchiveCache {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            slots: Vec::new(),
            #[cfg(test)]
            opens: 0,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Runs `f` on the archive at `path`, opening it unless a cached copy has the same
    /// size and modification time as the file now.
    ///
    /// # Errors
    ///
    /// When the file cannot be read or is not a zip. A cached copy of a file that has
    /// since vanished is dropped.
    pub fn with_archive<T>(
        &mut self,
        path: &Path,
        f: impl FnOnce(&mut Cached) -> T,
    ) -> Result<T, ArchiveError> {
        let found = self.slots.iter().position(|s| s.path == path);
        let stamp = match std::fs::metadata(path).and_then(|m| Stamp::of(&m)) {
            Ok(stamp) => stamp,
            Err(e) => {
                if let Some(i) = found {
                    self.slots.remove(i);
                }
                return Err(ArchiveError::Io(format!("{}: {e}", path.display())));
            }
        };
        let slot = match found {
            Some(i) if self.slots[i].stamp == stamp => self.slots.remove(i),
            found => {
                if let Some(i) = found {
                    self.slots.remove(i);
                }
                let file = File::open(path)
                    .map_err(|e| ArchiveError::Io(format!("{}: {e}", path.display())))?;
                let zip = ZipArchive::new(BufReader::new(file))?;
                #[cfg(test)]
                {
                    self.opens += 1;
                }
                let entries = entries(&zip);
                Slot {
                    path: path.to_path_buf(),
                    stamp,
                    cached: Cached { zip, entries },
                }
            }
        };
        self.slots.insert(0, slot);
        self.slots.truncate(self.capacity);
        Ok(f(&mut self.slots[0].cached))
    }

    #[cfg(test)]
    fn opens(&self) -> usize {
        self.opens
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    /// A fresh directory per test, so tests running in parallel never share a file.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nu_plugin_zip-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn write_zip(path: &Path, members: &[&str]) {
        let mut w = ZipWriter::new(File::create(path).expect("create zip"));
        for name in members {
            w.start_file(*name, SimpleFileOptions::default())
                .expect("start member");
            w.write_all(b"x").expect("write member");
        }
        w.finish().expect("finish zip");
    }

    fn names(cache: &mut ArchiveCache, path: &Path) -> Vec<String> {
        cache
            .with_archive(path, |c| c.entries.iter().map(|e| e.name.clone()).collect())
            .expect("open archive")
    }

    #[test]
    fn reuses_open_archive() {
        let path = scratch("reuse").join("a.zip");
        write_zip(&path, &["one.txt"]);
        let mut cache = ArchiveCache::new(16);
        assert!(cache.is_empty());
        assert_eq!(names(&mut cache, &path), ["one.txt"]);
        assert_eq!(names(&mut cache, &path), ["one.txt"]);
        assert_eq!(cache.opens(), 1);
        assert!(!cache.is_empty());
    }

    #[test]
    fn reopens_after_change() {
        let path = scratch("change").join("a.zip");
        write_zip(&path, &["one.txt"]);
        let mut cache = ArchiveCache::new(16);
        assert_eq!(names(&mut cache, &path), ["one.txt"]);
        write_zip(&path, &["one.txt", "two.txt"]);
        assert_eq!(names(&mut cache, &path), ["one.txt", "two.txt"]);
        assert_eq!(cache.opens(), 2);
    }

    /// A sync client or `rsync -t` replaces a file and keeps the source's mtime. When
    /// the size happens to match too, only the file's identity tells the two apart.
    #[cfg(unix)]
    #[test]
    fn reopens_after_replacement_with_same_size_and_mtime() {
        let dir = scratch("replace");
        let path = dir.join("a.zip");
        write_zip(&path, &["one.txt"]);
        let mut cache = ArchiveCache::new(16);
        assert_eq!(names(&mut cache, &path), ["one.txt"]);
        let next = dir.join("next.zip");
        write_zip(&next, &["two.txt"]);
        let old = std::fs::metadata(&path).expect("stat");
        assert_eq!(std::fs::metadata(&next).expect("stat").len(), old.len());
        File::options()
            .write(true)
            .open(&next)
            .and_then(|f| f.set_modified(old.modified()?))
            .expect("copy mtime");
        std::fs::rename(&next, &path).expect("replace");
        assert_eq!(names(&mut cache, &path), ["two.txt"]);
    }

    #[test]
    fn evicts_lru_beyond_capacity() {
        let dir = scratch("evict");
        let paths: Vec<PathBuf> = ["a", "b", "c"]
            .iter()
            .map(|n| dir.join(format!("{n}.zip")))
            .collect();
        for p in &paths {
            write_zip(p, &["m.txt"]);
        }
        let mut cache = ArchiveCache::new(2);
        for p in &paths {
            names(&mut cache, p);
        }
        assert_eq!(cache.opens(), 3);
        names(&mut cache, &paths[2]);
        assert_eq!(cache.opens(), 3, "the most recent stays cached");
        names(&mut cache, &paths[0]);
        assert_eq!(cache.opens(), 4, "the least recent was evicted");
    }

    #[test]
    fn missing_file_is_an_error() {
        let path = scratch("missing").join("nope.zip");
        let mut cache = ArchiveCache::new(16);
        assert!(cache.with_archive(&path, |_| ()).is_err());
        assert!(cache.is_empty());
    }
}
