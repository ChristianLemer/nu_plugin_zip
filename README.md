# nu_plugin_zip

**Browse zip archives from [Nushell](https://www.nushell.sh) like folders — `ls`, `glob` and
`open` inside a zip, in memory, never touching disk.**

```nushell
zip ls   data.zip reports                  # like ls, inside the archive
zip glob data.zip 'reports/**/*.json'      # like glob
zip open data.zip reports/2026.json        # like open: parsed by extension
open data.zip                              # every member as a table, via `from zip`
```

Nushell has no archive commands. `open data.zip` gives you bytes, and the usual way out is to
extract to a directory, or to call `tar -xOf` or `unzip -p` once per member. This plugin reads
the archive where it is, with the verbs you already use.

## The commands

**`zip ls <archive> [dir]`** — the direct children of a directory inside the archive, the root
if you name none, with `ls`'s columns: `name` (full path inside the archive), `type`, `size`
(uncompressed), `modified`. Directories the archive only implies — `a/b.txt` with no `a/`
entry — show up as `dir` all the same.

```nushell
zip ls data.zip | where type == file | sort-by size --reverse
zip ls data.zip reports/2026
```

**`zip glob <archive> <pattern>`** — the paths matching a pattern, as `glob` would: `*` stays
within one path segment, `**` crosses them, case matters, and paths use `/` on every host.
Directories match too; `--no-dir` (`-D`) leaves them out.

```nushell
zip glob data.zip 'reports/*'            # one level
zip glob data.zip '**/*.csv'             # any depth
zip glob data.zip '**/*' --no-dir        # every file
```

**`zip open <archive> ...paths`** — members, parsed like `open` parses a file. A `.json` member
goes through `from json`, a `.csv` through `from csv`: any `from` command in scope applies,
including another plugin's. With no parser for the extension you get a string when the bytes
are UTF-8, binary otherwise. `--raw` skips parsing.

```nushell
zip open data.zip config.json | get version
zip open --raw data.zip images/logo.png | save logo.png

# Several members in one call: a table of name and content
zip open data.zip reports/2025.json reports/2026.json | get content
```

One path gives the member itself, two or more give a table. The shape follows the number of
paths, so a spread list (`...$paths`) holding a single path returns the bare member, and an
empty one is an error.

**`from zip`** — what `open data.zip` calls: every file member as a row of `name type size
modified content`, the content as binary. Handy for small archives, or bytes that never were a
file:

```nushell
http get https://example.com/bundle.zip | from zip | where name =~ '\.md$'
```

**`to zip`** — the inverse of `from zip`, and what `save data.zip` calls: a table of `name` and
`content` becomes an archive, in memory. A string is written as text, binary as is, and any
other value through the `to` command its extension names, so a table under `rows.nuon` goes
through `to nuon`. A name ending in `/` is a directory; a file row needs a content, `""` for an
empty one. `modified` is kept when present, and `--mtime` gives every entry one date, for the
same bytes on every run:

```nushell
[[name content]; [notes/a.txt hello] [data/rows.nuon $rows]] | to zip | save notes.zip
open data.zip | where name !~ '^tmp/' | save trimmed.zip
$table | to zip --mtime 2026-01-01
```

Member paths are relative to the archive root and accept a leading `./` or `/`. The archive
path is relative to your current directory, as for any command.

## Nothing touches disk

No command writes a file, not even a temporary one. Members are decompressed into memory and
handed to Nushell as values, so the plugin is safe to use on data that must not be left behind
in plaintext.

## Stays loaded

Reading many members costs one process, not one per member. Measured on 85 members, a call
costs about a sixth of a `tar -xOf`, and most of that is decompression.

To make that hold, the plugin keeps opened archives — up to 16, keyed by path and reopened when
the file changes: its size or modification time, and on Linux and macOS its identity too, so
a file replaced by a sync client is never served from the old copy — and keeps itself running
while it holds one.
Nushell normally stops a plugin after 10 seconds idle; this one opts out of that while its cache
is not empty, so a `plugin_gc` setting for `zip` in your config has no effect then. `plugin stop
zip` still stops it.

A cached archive keeps its file open, so the drive holding it cannot be unmounted or ejected
while the plugin runs: the system reports it busy. `plugin stop zip` closes everything.

## Install

```nushell
http get https://github.com/ChristianLemer/nu_plugin_zip/raw/HEAD/install.nu
| save -f install.nu
nu install.nu --register
```

The installer reads the Nushell running it, downloads the matching build, verifies its
checksum and registers it. No Rust toolchain. Then add `plugin use zip` to your config so the
commands survive a restart — `plugin add` registers, it does not load.

Builds exist for the current Nushell minor and the ones before it that are still in use — the
exact list is [supported-nu.txt](supported-nu.txt). A plugin binary loads into exactly one
Nushell minor, so **re-run the installer after every Nushell upgrade**: the failure otherwise is
a mute `Failed to send plugin call`, never a version.

> **Arrived from crates.io?** Only the build for the newest supported Nushell is published
> there, because cargo ignores the `+nu-` part of a version. `cargo install nu_plugin_zip
> --locked` is the right route only when that newest version is also yours.

## Known issue: `source` and parsed members

In a file loaded with `source`, a `zip open` that parses its member — `data.json` through
`from json` — makes Nushell panic and exit. The plugin hands the bytes to the engine's parser,
and Nushell mishandles that call from a sourced file; the same line works in the REPL, in a
script run as `nu file.nu`, and inside a `def` or `do` block. Until Nushell fixes it, parse
outside the call in sourced files:

```nushell
zip open --raw data.zip config.json | decode | from json
```

## What it doesn't read

- **zstd, bzip2 and lzma members.** Their decoders would pull C libraries into the build.
  Stored, deflate and deflate64 cover what common tools produce. Others are an error naming the
  member and its method; listing the archive still works.
- **Encrypted members.** An error naming the member. No password is ever asked for.
- **Other formats.** tar, 7z, rar: a tar has no index, so reading one member means scanning the
  whole file, a different design.

Modifying an existing archive in place is out of scope too. [SPEC.md](SPEC.md) says why for
each.

## Contributing

Issues and pull requests are welcome. The gates CI runs, plus the check that one source tree
still serves every supported Nushell minor:

```bash
cargo fmt -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
./scripts/test-nu-compat.sh --all
```

[AGENTS.md](AGENTS.md) has the setup for that last one. [SPEC.md](SPEC.md) holds the design
decisions and their reasons.

## License

MIT
