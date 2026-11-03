# Exercise every command inside a real Nushell, through the plugin protocol.
#
# The unit tests call the Rust functions directly; nothing there proves that a
# binary loads into a given Nushell, answers on the wire, or that `zip open`
# reaches the engine's parsers. This script does. Run it with the plugin loaded
# for the session only, so a registered build cannot answer in place of the one
# under test:
#
#   nu --plugin-config <scratch> --plugins <path/to/nu_plugin_zip> -- scripts/smoke.nu
#
# Every case reads tests/fixtures/sample.zip, described in the README beside it.
use std/assert

let fixtures = ($env.FILE_PWD | path join .. tests fixtures | path expand)
let z = ($fixtures | path join sample.zip)

# zip ls: the root shows implied directories, a subdirectory its direct children.
assert equal (zip ls $z | get name) [bin data readme.txt]
assert equal (zip ls $z | get type) [dir dir file]
assert equal (zip ls $z data | length) 3
assert equal (zip ls $z ./data/ | get name) [data/a.json data/b.nuon data/deep]
let readme = (zip ls $z | where name == readme.txt | get 0)
assert equal $readme.size 6b
assert equal ($readme.modified | describe) "datetime"
assert (zip ls $z | get 0.modified | is-empty) "an implied directory has no date"
assert (try { zip ls $z nope; false } catch { true }) "a missing directory must be an error"

# zip glob: `*` stays in a segment, `**` crosses them.
assert equal (zip glob $z 'data/**/*.csv') [data/deep/c.csv]
assert equal (zip glob $z '*.txt') [readme.txt]
assert (try { zip glob $z '['; false } catch { true }) "an invalid pattern must be an error"

# zip open: parsed by extension through the engine's own `from` commands.
assert equal (zip open $z data/a.json) {x: 1}
assert equal (zip open $z data/b.nuon) {y: 2}
assert equal (zip open $z data/deep/c.csv) [[k v]; [1 2]]
assert equal (zip open $z readme.txt) "hello\n"
assert equal (zip open --raw $z bin/blob.bin | bytes length) 256
assert equal (zip open --raw $z readme.txt) 0x[68 65 6c 6c 6f 0a]
let several = (zip open $z data/a.json readme.txt)
assert equal ($several | get name) [data/a.json readme.txt]
assert equal ($several | get 0.content) {x: 1}
assert equal (zip open $z ./data/a.json /readme.txt | get name) [./data/a.json /readme.txt]
assert (try { zip open $z nope.txt; false } catch { true }) "a missing member must be an error"
assert (try { zip open $z data; false } catch { true }) "a directory must be an error"

# from zip: what `open x.zip` dispatches to.
assert equal (open $z | length) 5
assert equal (open $z | where name == readme.txt | get 0.content) 0x[68 65 6c 6c 6f 0a]

# The archive argument resolves against Nushell's directory, not the plugin's.
do {
  cd $fixtures
  assert equal (zip ls sample.zip | length) 3
}

# The plugin keeps itself alive while it caches an archive: the GC is told to
# stop idle plugins after one second, and the same process must still answer.
$env.config.plugin_gc = {default: {enabled: true, stop_after: 1sec}, plugins: {}}
zip ls $z | ignore
let pid = (plugin list | where name == zip | get 0.pid)
sleep 3sec
zip ls $z | ignore
assert equal (plugin list | where name == zip | get 0.pid) $pid "the plugin was stopped while it held a cached archive"

# The examples each command's --help shows. They are read back from Nushell
# rather than copied here, so the help text and this check cannot drift apart.
# Each runs in a fresh directory holding the fixture as archive.zip, in a child
# Nushell loading this same plugin through a throwaway registry, so a
# registered build cannot answer in its place.
let plugin = (plugin list | where name == zip | get 0.filename)
let commands = (scope commands | where name in ["zip ls" "zip glob" "zip open" "from zip"])
assert equal ($commands | length) 4
for cmd in $commands {
  assert ($cmd.examples | is-not-empty) $"`($cmd.name)` declares no examples"
  for ex in $cmd.examples {
    let dir = (mktemp --directory)
    cp $z ($dir | path join archive.zip)
    let script = $"cd r#'($dir)'#; ($ex.example) | describe"
    let run = (^$nu.current-exe --no-config-file --plugin-config ($dir | path join plugins.msgpackz) --plugins $plugin -c $script | complete)
    rm -r -f $dir
    assert equal $run.exit_code 0 $"example failed: ($ex.example)\n($run.stderr)"
  }
}

print $"smoke ok · Nushell ((version).version)"
