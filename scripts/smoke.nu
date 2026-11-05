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
assert equal (zip glob $z '**/*' --no-dir | length) 5
assert ((zip glob $z '**/*') | any {|p| $p == data/deep})
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

# to zip: the inverse of from zip, in memory; `save x.zip` dispatches to it.
let d = (mktemp --directory)
assert equal ([] | to zip | bytes length) 22
assert equal (open $z --raw | to zip) (open $z --raw) "binary input is an archive already, as `save` sends it"
assert equal (open $z | to zip | from zip | select name content) (open $z | select name content)
open $z | save ($d | path join b.zip)
assert equal (zip ls ($d | path join b.zip) | get name) (zip ls $z | get name)
[[name content]; [t.nuon [[x]; [1]]] [r.json {a: 1}]] | to zip | save ($d | path join s.zip)
assert equal (zip open ($d | path join s.zip) t.nuon) [[x]; [1]]
assert equal (zip open ($d | path join s.zip) r.json) {a: 1}
let table = [[name content]; [a.txt hello] [d/ ""]]
assert equal ($table | to zip --mtime 2026-01-01) ($table | to zip --mtime 2026-01-01)
let a = ($d | path join m1.zip)
$table | to zip --mtime 2026-01-01 | save -f $a
$table | to zip --mtime 2027-01-01 | save -f ($d | path join m2.zip)
assert equal (zip ls $a | where name == a.txt | get 0.modified | format date "%Y-%m-%d") "2026-01-01" "--mtime must set the entry date"
assert ((open --raw $a) != (open --raw ($d | path join m2.zip))) "different --mtime must give different bytes"
let tree = {"mybag/": {"bagit.txt": "BagIt-Version: 1.0", "data/": {"a.nuon": [[x]; [1]], "meta.nuon": {k: 2}, "forms/": {}}}}
$tree | to zip | save ($d | path join bag.zip)
assert equal (zip ls ($d | path join bag.zip) mybag/data | get name | sort) [mybag/data/a.nuon mybag/data/forms mybag/data/meta.nuon]
assert equal (zip open ($d | path join bag.zip) mybag/data/meta.nuon) {k: 2}
let forgot = (try { {data: {x: 1}} | to zip; "" } catch {|e| $e.rendered })
assert ($forgot | str contains "end it with /") "a forgotten / must say so"
assert (try { {"a/": "x"} | to zip; false } catch { true }) "a directory key needs a record"
assert (try { [[name content]; [Makefile [1]]] | to zip; false } catch { true }) "a value without an extension must be an error"
assert (try { [[name content]; [x.nosuchformat [1]]] | to zip; false } catch { true }) "a value without a `to` command must be an error"
assert (try { [[name content]; [../x a]] | to zip; false } catch { true }) "a `..` name must be an error"
rm -r -f $d

# zip create: a folder on disk, written through a temporary file renamed into place.
let d = (mktemp --directory)
mkdir ($d | path join bag/data/empty)
"BagIt-Version: 1.0" | save ($d | path join bag/bagit.txt)
{k: 1} | save ($d | path join bag/data/a.json)
let bz = ($d | path join bag.zip)
zip create $bz ($d | path join bag)
assert equal (zip ls $bz | get name) [bag]
assert equal (zip glob $bz '**/*' | length) 5
zip create $bz ($d | path join bag) --mtime 2026-01-01 --force
let first = (open --raw $bz)
zip create $bz ($d | path join bag) --mtime 2026-01-01 --force
assert equal (open --raw $bz) $first "--mtime must give the same bytes on every run"
assert (try { zip create $bz ($d | path join bag); false } catch { true }) "an existing dest needs --force"
assert equal (open --raw $bz) $first "a refused dest must be left as it was"
zip create ($d | path join two.zip) ($d | path join bag) ($d | path join bag/bagit.txt)
assert equal (zip ls ($d | path join two.zip) | get name) [bag bagit.txt]
zip create ($d | path join empty.zip)
assert equal (ls ($d | path join empty.zip) | get 0.size) 22B
# The cache holds bag.zip open; replacing it must work, and be seen.
zip ls $bz | ignore
"x" | save ($d | path join bag/new.txt)
zip create $bz ($d | path join bag) --force
assert equal (zip glob $bz '**/*' | length) 6
assert (try { zip create ($d | path join bag) ($d | path join bag/bagit.txt) --force; false } catch { true }) "a directory as dest must be an error"
assert (try { zip create ($d | path join nodir/x.zip) ($d | path join bag); false } catch { true }) "a missing parent must be an error"
assert equal (ls -a $d | where name =~ partial | length) 0 "no temporary file may be left behind"
rm -r -f $d

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
# Each runs in a fresh directory holding the fixture as archive.zip and a
# folder/ with one file, in a child Nushell loading this same plugin through
# a throwaway registry, so a registered build cannot answer in its place.
let plugin = (plugin list | where name == zip | get 0.filename)
let commands = (scope commands | where name in ["zip ls" "zip glob" "zip open" "from zip" "to zip" "zip create"])
assert equal ($commands | length) 6
for cmd in $commands {
  assert ($cmd.examples | is-not-empty) $"`($cmd.name)` declares no examples"
  for ex in $cmd.examples {
    let dir = (mktemp --directory)
    cp $z ($dir | path join archive.zip)
    mkdir ($dir | path join folder)
    "hello" | save ($dir | path join folder/a.txt)
    let script = $"cd r#'($dir)'#; ($ex.example) | describe"
    let run = (^$nu.current-exe --no-config-file --plugin-config ($dir | path join plugins.msgpackz) --plugins $plugin -c $script | complete)
    rm -r -f $dir
    assert equal $run.exit_code 0 $"example failed: ($ex.example)\n($run.stderr)"
  }
}

print $"smoke ok · Nushell ((version).version)"
