# nu_plugin_zip — specification

The authority for scope and command design. Consult it before expanding the surface.

## Intent

Nushell has no archive commands. `open` dispatches on extension, so `open x.zip` today returns
raw bytes, and the existing options either extract to disk (`nu_plugin_unzip`) or spawn an
external tool on every call (`tar -xOf`, `unzip -p`). A script that reads many members of an
archive pays a process start per member, and a pipeline that must not leave plaintext behind
cannot use extraction at all.

This plugin treats a zip as **a folder you browse with the verbs you already know**:
`ls`, `glob`, `open`, each prefixed with `zip`. Nothing is extracted; the only file ever written
is an archive you ask `zip create` for.

## Non-negotiable: nothing is extracted

**No command writes what it was not asked to write, and none extracts a member to disk.**
Members are read into memory and handed to Nushell as values. `zip create` writes exactly
one thing, the archive it is told to, through a temporary file beside it that is renamed
into place. A change that writes anything else is a change to this spec, not a detail.

This is the property that separates the plugin from extraction-based tools, and it is what
makes it usable on sensitive data. The README states it in the same words.

## Commands

| Command | Input → output | Does |
|---|---|---|
| `zip ls <archive> [dir]` | nothing → table | the direct children of `dir` inside the archive (root if omitted), like `ls` |
| `zip glob <archive> <pattern>` | nothing → list\<string\> | member paths matching `pattern`, like `glob`; `--no-dir` leaves directories out |
| `zip open <archive> <path>` | nothing → any | one member, parsed by its extension, like `open` |
| `zip open --raw <archive> ...paths` | nothing → binary, or table | one member as binary; several as `[name content]` rows, in one call |
| `from zip` | binary → table | every file member: `name type size modified content`. This is what `open x.zip` calls |
| `to zip` | table or record → binary | an archive in memory from `name content` rows, the inverse of `from zip`, or from a record shaped like a tree. This is what `save x.zip` calls |
| `zip create <dest> ...sources` | nothing → nothing | an archive at `dest` of files and directories on disk; `--force` replaces, `--mtime` fixes every date |

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

**`to zip` serializes like `save`.** It reads `name`, `content` and, when present and not null,
`modified`; other columns, such as the `type` and `size` of `from zip`, are ignored, so `open
a.zip | to zip` round-trips. A string content is written as its UTF-8 bytes and binary as is.
Any other value, records and tables included, goes through the `to <ext>` command its name's
extension picks (`a.nuon` → `to nuon`), whichever is in scope, built-in or a plugin's, the way
`zip open` parses by extension; its output, string or binary, is the content. No extension, or
no `to <ext>` in scope, is an error naming the row and the name. A name ending in `/` is a
directory, whose content must be absent, null or empty. A file row with no content, or a null
one, is an error, not an empty file: a missing column is far more often a mistake upstream than
a wish, and `""` says empty plainly. Rows are written in input order, every parent gets its own
directory entry, and an entry's date is `--mtime` if given, else its `modified`, else the time
of the call; `--mtime` makes the bytes the same on every run and every host. Empty input gives
the empty archive, 22 bytes. Binary input comes back unchanged: `save` sends a binary value
through `to <ext>` as it does any other, so `... | to zip | save x.zip` hands `to zip` its own
archive a second time; `--mtime` has no effect there. The archive is built in memory. Every
error from `to zip` carries a label on the call, so Nushell's output points at it.

**`to zip` takes a record as a tree.** The key says what an entry is, the value what it holds,
so that a hand-written fixture and generated `.nuon` data read the same way, and the value's
type never has to guess whether a record is a directory or a file to serialize:

| Key | Value | Becomes |
|---|---|---|
| ends in `/` | a record | a directory, the record's entries inside it |
| no trailing `/` | string, binary | a file, as is |
| no trailing `/` | anything else, record included | a file, serialized by extension |

- A key containing `/` elsewhere is split into a path: `{"data/a.json": …}` equals
  `{"data/": {"a.json": …}}`.
- A key ending in `/` whose value is not a record is an error.
- A key ending in more than one `/` (`{"a//": {}}`) is an error, as `a//b` is.
- A key with no extension and a value that is neither a string nor binary is an error: the
  name has no extension to pick a format. When the value is a record (`{data: {…}}`, most
  often a forgotten `/`), the error adds: end it with `/` if it is a directory.
- Order is the record's key order, depth-first. A record carries no dates: `--mtime`, else
  the current time. Duplicates and shared parents are handled as for rows, and names follow
  the rules shared by both writers, below; an error names the key path
  (`mybag/ › data/ › a.nuon`).

**`zip create` places like `cp -r`.** Each source lands at the archive root under its own
name: a directory becomes a root holding its whole tree, a file lands at the root. No flag strips
the top level; spreading a `glob` does it, and several sources mean several roots. `dest` and
every source resolve against Nushell's current directory; a source ending in `.` or `..` takes
the name of the directory it resolves to. The walk includes everything, hidden files too, in a
stable order: sources in argument order, each tree depth-first, a directory's entry before its
children, siblings sorted by name bytes. Every directory gets an explicit entry, empty ones
included. Files are streamed into the archive one at a time; memory does not grow with the tree.
An entry's date is `--mtime` if given, else the file's modification time in local time.

Refused, each an error naming `dest` and, when there is one, the offending path: `dest` an
existing directory, or in a directory that does not exist; `dest` existing, unless `--force`,
like `save`; `dest` inside one of the sources; a source missing or unreadable; a source that
is itself a symbolic link, or a link met in the walk, neither followed nor stored; a name that
is not valid UTF-8; anything neither a regular file nor a directory; two sources stored under
one name. Links in the path leading to a source are resolved as the OS resolves them. The
refusal holds for a tree that does not change while it is archived: a link swapped in during
the run is not detected, the same limit as `tar`, `zip` and `cp -r`.

**`zip create` writes atomically.** The archive goes to a dot-named temporary file in `dest`'s
directory, `.<dest name>.<random>.partial`, is synced, and is renamed onto `dest`. A destination
in a synced folder must never see a partial archive. On any error the temporary file is removed
and `dest` is untouched: neither created nor, with `--force`, damaged. Without `--force` the
rename itself refuses a `dest` that appeared meanwhile. Before the rename, `dest` is dropped
from the archive cache: the plugin may hold it open from an earlier read, and on Windows a
rename onto an open file fails. The archive is created as `save` creates a file, mode 0666
less the umask on Unix, so others can read it from a shared folder; `--force` does not keep an
existing `dest`'s mode. With `--force`, a symbolic link at `dest` is replaced by the archive,
never written through. Without `--force` the rename must not replace; a filesystem that cannot
rename without replacing refuses it, `dest` stays untouched, and `--force` is the way through.
The directory is not fsynced after the rename. Ctrl-C stops the write between entries, with the
same cleanup as any error. Known limit: a killed process leaves the temporary file behind,
recognisable by its name.

**Rules shared by both writers.** Entry names use `/` on every host and are stored as UTF-8.
Refused: an empty name; an absolute one (`/x`, `//x`, `C:/x`, `C:x`); one containing `\`,
refused rather than converted; any `..` segment; an empty segment (`a//b`); a `.` segment,
except one leading `./`, which is stripped. A name written twice, or as both a file and a
directory, is refused too. `zip create` takes its names from the disk and refuses one these
rules reject, naming the file: a `\` in a Unix file name, or a source named `a:b.txt`, which
reads as a drive. Every parent directory gets its own entry, written once and before its
children. A date is clamped into what a zip holds, 1980-01-01 00:00:00 to 2107-12-31 23:59:58,
and one outside is written as the nearest end; a `--mtime` outside 1980–2107 is an error
instead, since it was typed. Modes are fixed, 0644 for files and 0755 for directories, and every
entry is recorded as made on Unix, so the same input gives the same bytes on every host. Files
are deflated and directories stored; a file of 4 GiB or more is written as zip64.

**Paths** are relative to the archive root, use `/`, and accept a leading `./` or `/`. The
archive argument is resolved against Nushell's current directory, not the plugin's.

**Stored names are shown and reached as stored.** An archive can hold `../x` or `/etc/x`;
nothing here resolves such a name against the host, and every verb tries a path exactly as
stored before stripping a leading `./` or `/`. A leading `/` is part of the first segment, so
`zip ls` shows `/etc` at the root, `zip ls a.zip /etc` lists it, and `zip glob a.zip '/etc/*'`
matches in it. Hiding the one suspicious member from someone inspecting the archive would be
the worst outcome.

## Out of scope

- **Modifying an existing archive: adding, replacing or deleting members.** A different risk
  profile and a different command set; revisit only with a concrete user.
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
  re-reads the path's metadata (one `stat`); if the file changed, the entry is dropped and the
  archive reopened. Changed means another size or modification time, and on Unix another
  inode or ctime too: a sync client or `rsync -t` replaces a file and keeps its mtime, and
  when the size matches as well, only the file's identity tells the two apart. Windows offers
  no equivalent in stable Rust, so there the check stays at size and mtime. At most **16**
  archives are cached, least recently used evicted first.
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
