# nu_plugin_zip

Browse zip archives from [Nushell](https://www.nushell.sh) like folders — `ls`, `glob` and
`open` inside a zip, **in memory, never touching disk**.

> **Status: in development.** Nothing is released yet. The design is in [SPEC.md](SPEC.md).

```nu
zip ls  data.zip reports/                  # like ls, inside the archive
zip glob data.zip 'reports/**/*.json'      # like glob
zip open data.zip reports/2026.json        # like open: parsed by extension
open data.zip                              # every member as a table, via `from zip`
```

## Nothing touches disk

No command writes a file, not even a temporary one. Members are decompressed into memory and
handed to Nushell as values, so the plugin is safe to use on data that must not be left behind
in plaintext.

## License

MIT
