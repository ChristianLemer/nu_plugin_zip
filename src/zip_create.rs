//! `zip create`: an archive of files and directories on disk, written to `dest` through a
//! temporary file beside it that is renamed into place. The only code in the plugin that
//! writes to disk.

use std::collections::HashSet;
use std::fs::{File, Metadata};
use std::io::{BufWriter, ErrorKind};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDateTime};
use nu_plugin::{EngineInterface, EvaluatedCall, SimplePluginCommand};
use nu_protocol::{
    Category, Example, LabeledError, Signals, Signature, Spanned, SyntaxShape, Type, Value,
};

use crate::write::{check_mtime, Writer};
use crate::ZipPlugin;

pub struct ZipCreate;

#[allow(clippy::unnecessary_literal_bound)]
impl SimplePluginCommand for ZipCreate {
    type Plugin = ZipPlugin;

    fn name(&self) -> &str {
        "zip create"
    }

    fn description(&self) -> &str {
        "Write files and directories on disk into a new zip archive"
    }

    fn extra_description(&self) -> &str {
        "Each source lands at the archive root under its own name, as cp -r places it: a \
         directory brings its whole tree, hidden files included, every directory with its own \
         entry. To store a folder's contents without the folder, spread a glob. Symbolic links \
         are refused, never followed. Files are streamed one at a time. The archive is written \
         to a dot-named temporary file beside dest and renamed into place, so dest is never seen \
         half written; an existing dest is refused unless --force."
    }

    fn signature(&self) -> Signature {
        Signature::build("zip create")
            .input_output_type(Type::Nothing, Type::Nothing)
            .required("dest", SyntaxShape::Filepath, "The archive to write")
            .rest(
                "sources",
                SyntaxShape::Filepath,
                "Files and directories to store, each at the archive root",
            )
            .switch("force", "Replace dest if it exists", Some('f'))
            .named(
                "mtime",
                SyntaxShape::DateTime,
                "Give every entry this date, for byte-identical output",
                None,
            )
            .category(Category::FileSystem)
    }

    fn search_terms(&self) -> Vec<&str> {
        vec!["zip", "archive", "write", "compress", "create"]
    }

    fn examples(&self) -> Vec<Example<'_>> {
        vec![
            Example {
                description: "Archive a folder; the archive holds folder/ and its tree",
                example: "zip create folder.zip folder",
                result: None,
            },
            Example {
                description: "Archive a folder's contents, without the folder itself",
                example: "zip create flat.zip ...(glob folder/*)",
                result: None,
            },
            Example {
                description: "Replace an archive, with the same bytes on every run",
                example: "zip create folder.zip folder --force --mtime 2026-01-01",
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
        let arg: Spanned<String> = call.req(0)?;
        let args: Vec<String> = call.rest(1)?;
        let force = call.has_flag("force")?;
        let mtime = match call.get_flag_value("mtime") {
            Some(v) => Some(check_mtime(v.as_date()?.naive_local()).map_err(|e| {
                LabeledError::new(e.to_string()).with_label("outside what a zip holds", v.span())
            })?),
            None => None,
        };
        let cwd = PathBuf::from(engine.get_current_dir()?);
        // Lexically normalised, as the sources are.
        let dest: PathBuf = cwd.join(&arg.item).components().collect();
        let fail =
            |e: String| LabeledError::new(about(&dest, e)).with_label("refused here", call.head);
        check_dest(&dest, force).map_err(fail)?;
        let sources = sources(&cwd, &args, &dest).map_err(fail)?;
        write_atomic(&dest, force, |file| {
            write_archive(file, &sources, mtime, engine.signals())?;
            // The cache may hold the archive being replaced open, and on Windows a rename
            // onto an open file fails.
            if let Ok(canonical) = dest.canonicalize() {
                plugin
                    .forget(engine, &canonical)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .map_err(fail)?;
        Ok(Value::nothing(call.head))
    }
}

/// `e` led by `dest`, unless it already names it first.
fn about(dest: &Path, e: String) -> String {
    let dest = dest.display().to_string();
    if e.starts_with(&dest) {
        e
    } else {
        format!("{dest}: {e}")
    }
}

/// The checks that need nothing written: `dest` is not a directory, its directory exists,
/// and it does not exist yet unless `force`.
fn check_dest(dest: &Path, force: bool) -> Result<(), String> {
    if dest.is_dir() {
        return Err(format!("{}: is a directory", dest.display()));
    }
    absolute_dest(dest)?;
    if !force && std::fs::symlink_metadata(dest).is_ok() {
        return Err(already_exists(dest));
    }
    Ok(())
}

fn already_exists(dest: &Path) -> String {
    format!("{}: already exists; --force replaces it", dest.display())
}

/// Writes `dest` through a dot-named temporary file in its directory, synced and then renamed
/// into place. A synced folder must never see a partial archive at `dest`. Any error drops
/// the temporary file, which removes it, and leaves `dest` as it was. Without `force` the
/// rename refuses a `dest` that appeared meanwhile.
fn write_atomic(
    dest: &Path,
    force: bool,
    write: impl FnOnce(&mut File) -> Result<(), String>,
) -> Result<(), String> {
    let (Some(parent), Some(file_name)) = (dest.parent(), dest.file_name()) else {
        return Err(format!("{}: not a file path", dest.display()));
    };
    let prefix = format!(".{}.", file_name.to_string_lossy());
    let mut builder = tempfile::Builder::new();
    builder.prefix(&prefix).suffix(".partial");
    // tempfile creates 0600 and the rename keeps that mode. An archive is made to be read,
    // often by others from a shared folder: created as `save` creates a file, the umask decides.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut tmp = builder.tempfile_in(parent).map_err(|e| {
        format!(
            "{}: cannot create a temporary file beside it: {e}",
            dest.display()
        )
    })?;
    write(tmp.as_file_mut())?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| format!("{}: cannot write: {e}", dest.display()))?;
    let persisted = if force {
        tmp.persist(dest)
    } else {
        tmp.persist_noclobber(dest)
    };
    persisted.map(drop).map_err(|e| {
        if !force && e.error.kind() == ErrorKind::AlreadyExists {
            already_exists(dest)
        } else {
            format!("{}: cannot rename into place: {}", dest.display(), e.error)
        }
    })
}

/// Streams every source into `file` as an archive, opening files one at a time. Each entry
/// is dated `mtime` when given, else by its modification time. Ctrl-C stops it between
/// entries.
fn write_archive(
    file: &mut File,
    sources: &[Source],
    mtime: Option<NaiveDateTime>,
    signals: &Signals,
) -> Result<(), String> {
    let mut w = Writer::new(BufWriter::new(file));
    for s in sources {
        if signals.interrupted() {
            return Err("interrupted; nothing was written".to_string());
        }
        let date = mtime.unwrap_or(s.modified);
        if s.is_dir {
            w.dir(&s.name, date).map_err(|e| e.to_string())?;
        } else {
            let mut f = File::open(&s.disk)
                .map_err(|e| format!("{}: cannot open: {e}", s.disk.display()))?;
            w.file(&s.name, date, s.size, &mut f)
                .map_err(|e| format!("{}: {e}", s.disk.display()))?;
        }
    }
    w.finish()
        .map_err(|e| e.to_string())?
        .into_inner()
        .map_err(|e| format!("cannot write: {}", e.error()))?;
    Ok(())
}

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
    use std::io::Write;

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

    fn partials(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .expect("read dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".partial"))
            .collect()
    }

    #[test]
    fn atomic_write_leaves_nothing_on_error() {
        let s = Scratch::new("atomic_err");
        let root = s.join("");
        let dest = root.join("out.zip");
        let err = write_atomic(&dest, false, |f| {
            f.write_all(b"half").map_err(|e| e.to_string())?;
            Err("boom".to_string())
        })
        .expect_err("closure failed");
        assert!(err.contains("boom"), "{err}");
        assert!(!dest.exists());
        assert_eq!(partials(&root), Vec::<String>::new());
    }

    #[test]
    fn atomic_write_keeps_dest_on_error_with_force() {
        let s = Scratch::new("atomic_force_err");
        let root = s.join("");
        let dest = root.join("out.zip");
        fs::write(&dest, b"original").expect("write");
        write_atomic(&dest, true, |_| Err("boom".to_string())).expect_err("closure failed");
        assert_eq!(fs::read(&dest).expect("read"), b"original");
        assert_eq!(partials(&root), Vec::<String>::new());
    }

    #[test]
    fn atomic_write_replaces_with_force() {
        let s = Scratch::new("atomic_force");
        let root = s.join("");
        let dest = root.join("out.zip");
        fs::write(&dest, b"original").expect("write");
        write_atomic(&dest, true, |f| {
            f.write_all(b"new").map_err(|e| e.to_string())
        })
        .expect("replaced");
        assert_eq!(fs::read(&dest).expect("read"), b"new");
        assert_eq!(partials(&root), Vec::<String>::new());
    }

    #[test]
    fn no_force_refuses_existing() {
        let s = Scratch::new("atomic_noclobber");
        let root = s.join("");
        let dest = root.join("out.zip");
        fs::write(&dest, b"original").expect("write");
        let err = write_atomic(&dest, false, |f| {
            f.write_all(b"new").map_err(|e| e.to_string())
        })
        .expect_err("exists");
        assert!(err.starts_with(&dest.display().to_string()), "{err}");
        assert_eq!(fs::read(&dest).expect("read"), b"original");
        assert_eq!(partials(&root), Vec::<String>::new());
    }

    #[test]
    fn dest_is_a_directory() {
        let s = Scratch::new("dest_dir");
        let root = tree(&s);
        let dest = root.join("bag");
        for force in [false, true] {
            let err = check_dest(&dest, force).expect_err("a directory");
            assert!(err.starts_with(&dest.display().to_string()), "{err}");
            assert!(err.contains("directory"), "{err}");
        }
    }

    #[test]
    fn dest_parent_missing() {
        let s = Scratch::new("dest_noparent");
        let root = s.join("");
        let dest = root.join("nodir/out.zip");
        let err = check_dest(&dest, false).expect_err("no parent");
        assert!(err.starts_with(&dest.display().to_string()), "{err}");
    }

    #[test]
    fn dest_exists_needs_force() {
        let s = Scratch::new("dest_exists");
        let root = tree(&s);
        let dest = root.join("notes.txt");
        let err = check_dest(&dest, false).expect_err("exists");
        assert!(err.starts_with(&dest.display().to_string()), "{err}");
        assert!(err.contains("--force"), "{err}");
        assert!(check_dest(&dest, true).is_ok());
        assert!(check_dest(&root.join("new.zip"), false).is_ok());
    }

    #[test]
    fn archive_of_a_tree_reads_back() {
        let s = Scratch::new("write_tree");
        let root = tree(&s);
        let dest = root.join("out.zip");
        let srcs = sources(&root, &args(&["bag", "notes.txt"]), &dest).expect("walk");
        write_atomic(&dest, false, |f| {
            write_archive(f, &srcs, None, &Signals::empty())
        })
        .expect("write");
        let zip = zip::ZipArchive::new(fs::File::open(&dest).expect("open")).expect("zip");
        let names: Vec<String> = (0..zip.len())
            .map(|i| {
                zip.name_for_index(i)
                    .expect("index")
                    .expect("name")
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            [
                "bag/",
                "bag/a/",
                "bag/a/z.txt",
                "bag/b.txt",
                "bag/c/",
                "notes.txt"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn archive_mode_follows_the_umask() {
        use std::os::unix::fs::PermissionsExt;
        let s = Scratch::new("write_mode");
        let root = s.join("");
        let reference = root.join("reference");
        fs::File::create(&reference).expect("create");
        let mode = |p: &Path| fs::metadata(p).expect("stat").permissions().mode() & 0o777;
        let dest = root.join("out.zip");
        write_atomic(&dest, false, |_| Ok(())).expect("write");
        assert_eq!(
            mode(&dest),
            mode(&reference),
            "0o666 & !umask, as File::create"
        );
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).expect("chmod");
        write_atomic(&dest, true, |_| Ok(())).expect("replace");
        assert_eq!(
            mode(&dest),
            mode(&reference),
            "--force does not keep the old mode"
        );
    }

    #[test]
    fn interrupt_stops_the_write() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let s = Scratch::new("write_interrupt");
        let root = tree(&s);
        let dest = root.join("out.zip");
        let srcs = sources(&root, &args(&["bag"]), &dest).expect("walk");
        let signals = Signals::new(Arc::new(AtomicBool::new(true)));
        let err = write_atomic(&dest, false, |f| write_archive(f, &srcs, None, &signals))
            .expect_err("interrupted");
        assert!(err.contains("interrupted"), "{err}");
        assert!(!dest.exists());
        assert_eq!(partials(&root), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_names_its_path_and_leaves_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let s = Scratch::new("write_unreadable");
        let root = tree(&s);
        let locked = root.join("bag/b.txt");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        if fs::File::open(&locked).is_ok() {
            return; // running as root: permissions do not bind
        }
        let dest = root.join("out.zip");
        let srcs = sources(&root, &args(&["bag"]), &dest).expect("the walk never opens files");
        let err = write_atomic(&dest, false, |f| {
            write_archive(f, &srcs, None, &Signals::empty())
        })
        .expect_err("unreadable");
        assert!(err.contains(&locked.display().to_string()), "{err}");
        assert!(!dest.exists());
        assert_eq!(partials(&root), Vec::<String>::new());
    }

    #[test]
    fn missing_source_refused() {
        let s = Scratch::new("walk_missing");
        let root = s.join("");
        let err = sources(&root, &args(&["nope"]), &root.join("o.zip")).expect_err("missing");
        assert!(err.contains("nope"), "{err}");
    }
}
