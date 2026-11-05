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
| `zip glob <archive> <pattern>` | nothing → list\<string\> | member paths matching `pattern`, like `glob`; `--no-dir` leaves directories out |
| `zip open <archive> <path>` | nothing → any | one member, parsed by its extension, like `open` |
| `zip open --raw <archive> ...paths` | nothing → binary, or table | one member as binary; several as `[name content]` rows, in one call |
| `from zip` | binary → table | every file member: `name type size modified content`. This is what `open x.zip` calls |
| `to zip` | table or record → binary | an archive in memory from `name content` rows, the inverse of `from zip`, or from a record shaped like a tree. This is what `save x.zip` calls |

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
  the rules for every archive written, below; an error names the key path
  (`mybag/ › data/ › a.nuon`).

**Rules for every archive written.** Entry names use `/` on every host and are stored as UTF-8.
Refused: an empty name; an absolute one (`/x`, `//x`, `C:/x`, `C:x`); one containing `\`,
refused rather than converted; any `..` segment; an empty segment (`a//b`); a `.` segment,
except one leading `./`, which is stripped. A name written twice, or as both a file and a
directory, is refused too. Every parent directory gets its own entry, written once and before
its children. A date is clamped into what a zip holds, 1980-01-01 00:00:00 to 2107-12-31
23:59:58, and one outside is written as the nearest end; a `--mtime` outside 1980–2107 is an
error instead, since it was typed. Modes are fixed, 0644 for files and 0755 for directories,
and every entry is recorded as made on Unix, so the same input gives the same bytes on every
host. Files are deflated and directories stored; a file of 4 GiB or more is written as zip64.

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
