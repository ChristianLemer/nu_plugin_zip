# Test fixtures

`sample.zip` is committed, not generated: the smoke test runs inside a real Nushell, which
cannot create a zip, and no zip tool is guaranteed on every CI host. It was written once with
Python's `zipfile`, every member dated 2026-01-02 03:04:06:

| Member | Method | Content |
|---|---|---|
| `readme.txt` | stored | `hello\n` |
| `data/a.json` | deflate | `{"x": 1}` |
| `data/b.nuon` | stored | `{y: 2}` |
| `data/deep/c.csv` | deflate | `k,v\n1,2\n` |
| `bin/blob.bin` | deflate | bytes 0 to 255 |

There is deliberately no `data/` or `bin/` directory entry: those directories are only
implied by member names, which `zip ls` must still list.

`golden.zip` is what `write::Writer` produces for `readme.txt` and `data/a.json` (with its implied `data/` entry), dated 2026-01-01 00:00:00. A test compares the writer's output to it byte for byte, on every CI host; a diff means the `zip` crate changed what it writes.
