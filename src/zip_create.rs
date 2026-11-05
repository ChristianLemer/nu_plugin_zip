//! `zip create`: the walk that turns path arguments into the list of entries to write.

// Nothing calls this until the command lands.
#![allow(dead_code)]

use std::collections::HashSet;
use std::fs::Metadata;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDateTime};

/// One entry to write: where it lives on disk, and what it is called in the archive.
#[derive(Debug)]
pub(crate) struct Source {
    pub disk: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: NaiveDateTime,
}

/// Every entry to write, in order: sources in argument order, each tree depth-first,
/// a directory before its children, siblings sorted by file name bytes.
///
/// Placement follows `cp -r`: a source lands at the archive root under its own name, and a
/// directory brings its tree along. A source that is itself a link, or a link met in the
/// walk, is refused, never followed nor stored; links in the path leading to a source are
/// resolved as the OS resolves them. The check holds for a tree that does not change while
/// it is read: a link swapped in meanwhile is not detected, as with tar, zip or `cp -r`.
///
/// `dest` is absolute but may not exist yet.
pub(crate) fn sources(cwd: &Path, args: &[String], dest: &Path) -> Result<Vec<Source>, String> {
    let dest_abs = absolute_dest(dest)?;
    let mut out: Vec<Source> = Vec::new();
    let mut roots: HashSet<String> = HashSet::new();
    for arg in args {
        // Lexically normalised, so a trailing `/` or `/.` cannot make the OS resolve a link
        // before it is looked at.
        let disk: PathBuf = cwd.join(arg).components().collect();
        let meta =
            std::fs::symlink_metadata(&disk).map_err(|e| format!("{}: {e}", disk.display()))?;
        if meta.file_type().is_symlink() {
            return Err(refused_symlink(&disk));
        }
        let canonical = disk
            .canonicalize()
            .map_err(|e| format!("{}: {e}", disk.display()))?;
        let name = file_name(&canonical)?;
        if dest_abs.starts_with(&canonical) {
            return Err(format!(
                "{}: the archive being written is inside {}",
                dest.display(),
                disk.display()
            ));
        }
        if !roots.insert(name.clone()) {
            return Err(format!(
                "{}: another source is already stored as `{name}`",
                disk.display()
            ));
        }
        walk(&disk, &name, &meta, &mut out)?;
    }
    Ok(out)
}

/// `dest` with its parent canonicalized, so a link or `..` cannot hide it inside a source.
/// The parent must exist; the file itself need not.
fn absolute_dest(dest: &Path) -> Result<PathBuf, String> {
    let (Some(parent), Some(file)) = (dest.parent(), dest.file_name()) else {
        return Err(format!("{}: not a file path", dest.display()));
    };
    let parent = parent.canonicalize().map_err(|e| {
        format!(
            "{}: the directory it would go in does not exist ({}: {e})",
            dest.display(),
            parent.display()
        )
    })?;
    Ok(parent.join(file))
}

fn refused_symlink(path: &Path) -> String {
    format!(
        "{}: is a symbolic link, which zip create neither follows nor stores",
        path.display()
    )
}

fn file_name(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("{}: has no file name to store it under", path.display()))?;
    name.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{}: the name is not valid UTF-8", path.display()))
}

fn modified(meta: &Metadata, path: &Path) -> Result<NaiveDateTime, String> {
    let time = meta
        .modified()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(DateTime::<Local>::from(time).naive_local())
}

fn walk(disk: &Path, name: &str, meta: &Metadata, out: &mut Vec<Source>) -> Result<(), String> {
    let is_dir = meta.is_dir();
    if !is_dir && !meta.is_file() {
        return Err(format!(
            "{}: neither a file nor a directory",
            disk.display()
        ));
    }
    out.push(Source {
        disk: disk.to_path_buf(),
        name: name.to_string(),
        is_dir,
        size: if is_dir { 0 } else { meta.len() },
        modified: modified(meta, disk)?,
    });
    if !is_dir {
        return Ok(());
    }
    let mut children = Vec::new();
    for entry in std::fs::read_dir(disk).map_err(|e| format!("{}: {e}", disk.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", disk.display()))?;
        let child = entry.path();
        children.push((file_name(&child)?, child));
    }
    children.sort_by(|a, b| a.0.cmp(&b.0));
    for (child_name, child) in children {
        let meta =
            std::fs::symlink_metadata(&child).map_err(|e| format!("{}: {e}", child.display()))?;
        if meta.file_type().is_symlink() {
            return Err(refused_symlink(&child));
        }
        walk(&child, &format!("{name}/{child_name}"), &meta, out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;
    use std::fs;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    fn tree(s: &Scratch) -> PathBuf {
        let root = s.join("");
        fs::create_dir_all(root.join("bag/a")).expect("mkdir");
        fs::create_dir_all(root.join("bag/c")).expect("mkdir");
        fs::write(root.join("bag/b.txt"), b"bb").expect("write");
        fs::write(root.join("bag/a/z.txt"), b"z").expect("write");
        fs::write(root.join("notes.txt"), b"n").expect("write");
        root
    }

    #[test]
    fn walk_order_and_names() {
        let s = Scratch::new("walk_order");
        let root = tree(&s);
        let out = sources(&root, &args(&["bag"]), &root.join("out.zip")).expect("walk");
        let got: Vec<_> = out.iter().map(|x| (x.name.as_str(), x.is_dir)).collect();
        assert_eq!(
            got,
            [
                ("bag", true),
                ("bag/a", true),
                ("bag/a/z.txt", false),
                ("bag/b.txt", false),
                ("bag/c", true)
            ]
        );
        assert_eq!(out[3].size, 2);
    }

    #[test]
    fn walk_names_use_slash() {
        let s = Scratch::new("walk_slash");
        let root = tree(&s);
        let out = sources(&root, &args(&["bag"]), &root.join("out.zip")).expect("walk");
        assert!(out.iter().all(|x| !x.name.contains('\\')));
        assert!(out.iter().any(|x| x.name == "bag/a/z.txt"));
    }

    #[test]
    fn file_source_lands_at_root() {
        let s = Scratch::new("walk_file");
        let root = tree(&s);
        let out = sources(&root, &args(&["notes.txt"]), &root.join("out.zip")).expect("walk");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "notes.txt");
        assert!(!out[0].is_dir);
    }

    #[test]
    fn several_roots() {
        let s = Scratch::new("walk_roots");
        let root = tree(&s);
        let out =
            sources(&root, &args(&["./bag", "notes.txt"]), &root.join("out.zip")).expect("walk");
        let roots: Vec<_> = out
            .iter()
            .filter(|x| !x.name.contains('/'))
            .map(|x| x.name.as_str())
            .collect();
        assert_eq!(roots, ["bag", "notes.txt"]);
        assert_eq!(out.last().expect("last").name, "notes.txt");
    }

    #[test]
    fn dot_takes_the_dir_name() {
        let s = Scratch::new("walk_dot");
        let root = tree(&s);
        let cwd = root.join("bag");
        let out = sources(&cwd, &args(&["."]), &root.join("out.zip")).expect("walk");
        assert_eq!(out[0].name, "bag");
        assert_eq!(out[1].name, "bag/a");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_refused() {
        let s = Scratch::new("walk_symlink");
        let root = tree(&s);
        std::os::unix::fs::symlink("notes.txt", root.join("bag/lnk")).expect("link");
        let err = sources(&root, &args(&["bag"]), &root.join("out.zip")).expect_err("inside");
        assert!(err.contains("lnk"), "{err}");
        std::os::unix::fs::symlink("bag", root.join("top")).expect("link");
        let err = sources(&root, &args(&["top"]), &root.join("out.zip")).expect_err("source");
        assert!(err.contains("top"), "{err}");
        for arg in ["top/", "top/."] {
            let err = sources(&root, &args(&[arg]), &root.join("out.zip")).expect_err(arg);
            assert!(
                err.contains("top") && err.contains("symbolic"),
                "{arg}: {err}"
            );
        }
    }

    #[test]
    fn dest_equal_to_a_file_source_refused() {
        let s = Scratch::new("walk_dest_file");
        let root = tree(&s);
        let err = sources(&root, &args(&["notes.txt"]), &root.join("notes.txt")).expect_err("eq");
        assert!(err.contains("notes.txt"), "{err}");
    }

    #[test]
    fn dest_parent_missing_names_dest() {
        let s = Scratch::new("walk_dest_parent");
        let root = tree(&s);
        let err = sources(&root, &args(&["bag"]), &root.join("nodir/bag.zip")).expect_err("dest");
        assert!(
            err.starts_with(&root.join("nodir/bag.zip").display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn dest_inside_source_refused() {
        let s = Scratch::new("walk_dest");
        let root = tree(&s);
        let err = sources(&root, &args(&["bag"]), &root.join("bag/a/out.zip")).expect_err("dest");
        assert!(err.contains("out.zip"), "{err}");
        // beside the source is fine
        assert!(sources(&root, &args(&["bag"]), &root.join("bagout.zip")).is_ok());
    }

    #[test]
    fn same_name_twice_refused() {
        let s = Scratch::new("walk_twice");
        let root = s.join("");
        fs::create_dir_all(root.join("a")).expect("mkdir");
        fs::create_dir_all(root.join("b")).expect("mkdir");
        fs::write(root.join("a/x"), b"1").expect("write");
        fs::write(root.join("b/x"), b"2").expect("write");
        let err = sources(&root, &args(&["./a/x", "./b/x"]), &root.join("o.zip")).expect_err("dup");
        // The disk path, as the host writes it: `b\x` on Windows.
        let second = Path::new("b").join("x").display().to_string();
        assert!(err.contains("`x`") && err.contains(&second), "{err}");
    }

    #[test]
    fn missing_source_refused() {
        let s = Scratch::new("walk_missing");
        let root = s.join("");
        let err = sources(&root, &args(&["nope"]), &root.join("o.zip")).expect_err("missing");
        assert!(err.contains("nope"), "{err}");
    }
}
