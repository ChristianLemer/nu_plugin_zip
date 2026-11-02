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
