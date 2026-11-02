pub mod archive;
pub mod cache;
mod from_zip;
mod zip_glob;
mod zip_ls;
mod zip_open;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use nu_plugin::{EngineInterface, Plugin, PluginCommand};
use nu_protocol::LabeledError;

use crate::cache::{ArchiveCache, Cached};

/// How many archives stay open between calls (`SPEC.md`, *Performance contract*).
const CACHE_CAPACITY: usize = 16;

pub struct ZipPlugin {
    cache: Mutex<ArchiveCache>,
}

impl ZipPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(ArchiveCache::new(CACHE_CAPACITY)),
        }
    }

    /// Runs `f` on the archive at `path`, through the cache. Nushell's garbage collector
    /// would stop an idle plugin and throw the cache away, so the plugin opts out of it
    /// while the cache holds an archive, and back in once it is empty.
    ///
    /// # Errors
    ///
    /// When the archive cannot be opened, or the engine refuses the GC setting.
    pub fn with_archive<T>(
        &self,
        engine: &EngineInterface,
        path: &Path,
        f: impl FnOnce(&mut Cached) -> T,
    ) -> Result<T, LabeledError> {
        // A panic while the lock was held leaves a cache that is still consistent: every
        // mutation is a whole slot moved, inserted or dropped.
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let result = cache.with_archive(path, f);
        engine.set_gc_disabled(!cache.is_empty())?;
        result.map_err(|e| LabeledError::new(e.to_string()))
    }
}

impl Default for ZipPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for ZipPlugin {
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").into()
    }

    fn commands(&self) -> Vec<Box<dyn PluginCommand<Plugin = Self>>> {
        vec![
            Box::new(zip_ls::ZipLs),
            Box::new(zip_glob::ZipGlob),
            Box::new(zip_open::ZipOpen),
            Box::new(from_zip::FromZip),
        ]
    }
}

/// The archive argument as a canonical path, resolved against Nushell's current
/// directory rather than the plugin process's, which never follows `cd`.
///
/// # Errors
///
/// When the engine cannot report its directory, or the path does not exist.
pub fn resolve(engine: &EngineInterface, arg: &str) -> Result<PathBuf, LabeledError> {
    let cwd = engine.get_current_dir()?;
    resolve_in(Path::new(&cwd), arg).map_err(|e| LabeledError::new(format!("{arg}: {e}")))
}

fn resolve_in(cwd: &Path, arg: &str) -> std::io::Result<PathBuf> {
    cwd.join(arg).canonicalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_archive_resolves_against_the_shell_cwd() {
        let dir =
            std::env::temp_dir().join(format!("nu_plugin_zip-{}-resolve", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).expect("scratch dir");
        std::fs::write(dir.join("sub/a.zip"), b"").expect("touch");
        let canonical = dir.join("sub/a.zip").canonicalize().expect("canonical");
        assert_eq!(resolve_in(&dir, "sub/a.zip").expect("relative"), canonical);
        assert_eq!(
            resolve_in(&dir, "./sub/../sub/a.zip").expect("dotted"),
            canonical
        );
        assert_eq!(
            resolve_in(Path::new("/elsewhere"), canonical.to_str().expect("utf-8"))
                .expect("absolute"),
            canonical
        );
        assert!(resolve_in(&dir, "missing.zip").is_err());
    }
}
