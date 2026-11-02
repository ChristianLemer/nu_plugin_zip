# nu_plugin_zip — specification

The authority for scope and command design. Consult it before expanding the surface.

## Intent

Nushell has no archive commands. `open` dispatches on extension, so `open x.zip` today returns
raw bytes, and the existing options either extract to disk (`nu_plugin_unzip`) or spawn an
external tool on every call (`tar -xOf`, `unzip -p`). A script that reads many members of an
archive pays a process start per member, and a pipeline that must not leave plaintext behind
cannot use extraction at all.

This plugin treats a zip as **a read-only folder you browse with the verbs you already know**:
`ls`, `glob`, `open`, each prefixed with `zip`. Nothing is extracted. Nothing is written.

## Non-negotiable: nothing touches disk

**No command writes to disk, ever, not even a temporary file.** Members are decompressed into
memory and handed to Nushell as values. This is the property that separates the plugin from
extraction-based tools, and it is what makes it usable on sensitive data. The README states it
in the same words. A change that needs a temporary file is a change to this spec, not a detail.

## Commands

| Command | Input → output | Does |
|---|---|---|
| `zip ls <archive> [dir]` | nothing → table | the direct children of `dir` inside the archive (root if omitted), like `ls` |
| `zip glob <archive> <pattern>` | nothing → list\<string\> | member paths matching `pattern`, like `glob` |
| `zip open <archive> <path>` | nothing → any | one member, parsed by its extension, like `open` |
| `zip open --raw <archive> ...paths` | nothing → binary, or table | one member as binary; several as `[name content]` rows, in one call |
| `from zip` | binary → table | every file member: `name type size modified content`. This is what `open x.zip` calls |

**`zip ls` columns** match `ls`: `name` (full path inside the archive), `type` (`file` or
`dir`), `size` (filesize, uncompressed), `modified` (datetime). Directories the archive only
implies — `a/b.txt` with no `a/` entry — are listed as `dir` all the same, since that is how
every archive tool shows them.

**Patterns** follow Nushell's `glob`: `*` stays within one path segment, `**` crosses them,
matching is case-sensitive, and paths use `/` whatever the host.

**`zip open` parses like `open`.** For `data/a.json` it calls the engine's `from json` on the
member's bytes, so any `from` command in scope — built-in or another plugin's — applies. The
bytes go as a byte stream of unknown type, which is how `open` hands over a file: sent as a
binary value, they would be refused by the parsers that take text, `from json` first. With no
`from <ext>` in scope the bytes come back as a string when they are valid UTF-8, binary
otherwise, which is what `open` does. `--raw` skips parsing.

**Paths** are relative to the archive root, use `/`, and accept a leading `./` or `/`. The
archive argument is resolved against Nushell's current directory, not the plugin's.

**Stored names are shown and reached as stored.** An archive can hold `../x` or `/etc/x`;
nothing here resolves such a name against the host, and every verb tries a path exactly as
stored before stripping a leading `./` or `/`. A leading `/` is part of the first segment, so
`zip ls` shows `/etc` at the root, `zip ls a.zip /etc` lists it, and `zip glob a.zip '/etc/*'`
matches in it. Hiding the one suspicious member from someone inspecting the archive would be
the worst outcome.

## Out of scope

- **Writing archives.** `to zip`, adding or deleting members. A different risk profile and a
  different command set; revisit only with a concrete user.
- **Other formats.** tar, 7z, rar. The command prefix is `zip`, and a tar has no index, so a
  random-access read means a full scan — a different design.
- **Encrypted members.** Reported as an error naming the member, never prompted for.
- **zstd, bzip2, lzma members.** Their Rust decoders pull C libraries or weight the build;
  `stored`, `deflate` and `deflate64` cover what common tools produce. Others are an error
  naming the member and its method.
- **Mounting, `cd` into an archive.** The plugin protocol offers no hook into `ls`, `cd` or
  `open`. If Nushell ever grows one, these verbs are meant to map onto it unchanged.

## Performance contract

The plugin exists so that reading N members costs one process, not N.

- **The process stays.** Nushell's plugin garbage collector stops an idle plugin after 10 s by
  default, which would throw away the cache below and charge the next call a process start.
  The plugin therefore calls `set_gc_disabled(true)` while its cache holds an archive, and
  `set_gc_disabled(false)` when the cache empties. A user's `plugin_gc` setting for `zip`
  cannot override this; document it in the README.
- **The index is read once.** Opened archives are cached, keyed by canonical path. Each call
  re-reads the path's size and modification time (one `stat`); if either changed, the entry is
  dropped and the archive reopened. At most **16** archives are cached, least recently used
  evicted first.
- **A member read decompresses that member only.**

## Nushell version compatibility

The plugin protocol version is a compile-time constant and Nushell rejects a plugin outside a
caret match, so every Nushell minor is a hard break and one binary serves exactly one minor.
The release carries one variant per supported minor, named by build metadata
(`0.1.0+nu-0.115.1`). `supported-nu.txt` lists the minors; `AGENTS.md` holds the release
procedure. One source tree serves every supported minor: prefer a fix that compiles everywhere
over version-gated code.

## Open questions

- Whether `zip ls` should also accept a glob in place of `dir`, as `ls` does. Not until a
  user asks: `zip glob` covers it.
