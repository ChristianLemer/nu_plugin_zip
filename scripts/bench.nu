# Measure what the plugin exists for: reading many members of one archive
# without a process start per member.
#
# Generates a synthetic zip (85 deflated CSV members of made-up rows, no real
# data) under target/bench/, then times three ways of reading every member:
#
#   tar      `bsdtar -xOf` once per member, two passes   (skipped without bsdtar)
#   plugin   `zip open --raw` once per member, two passes
#   batch    one `zip open --raw` naming all 85 members
#
# The second pass shows what the cache buys: the archive index is read once.
# Run it with the plugin under test loaded for the session:
#
#   nu --plugin-config <scratch> --plugins <plugin binary> -- scripts/bench.nu
#
# Measure an optimized build: in debug, decompression alone costs more than a
# process start and the comparison says nothing. Release without LTO builds in
# about a minute and decompresses like the shipped binary:
#
#   CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
#     cargo build --release --target-dir target/bench-build

let dir = ($env.FILE_PWD | path join .. target bench | path expand)
mkdir $dir
let z = ($dir | path join bench.zip)

let generate = "
import random, sys, zipfile
random.seed(85)
words = ['alpha', 'beta', 'gamma', 'delta', 'epsilon', 'zeta', 'eta', 'theta']
with zipfile.ZipFile(sys.argv[1], 'w', zipfile.ZIP_DEFLATED) as z:
    for i in range(85):
        rows = ['id,name,value'] + [
            f'{n},{random.choice(words)},{random.random():.6f}' for n in range(4000)
        ]
        z.writestr(f'members/m{i:03}.csv', '\\n'.join(rows) + '\\n')
"
^python3 -c $generate $z

let members = (zip glob $z 'members/*')
let size = (ls $z | get 0.size)
print $"archive: ($members | length) members, ($size) compressed"

def per-member [read: closure] {
  timeit { for m in $members { do $read $m | ignore } }
}

mut rows = []
if (which bsdtar | is-not-empty) {
  for pass in [1 2] {
    let t = (per-member {|m| ^bsdtar -xOf $z $m })
    $rows = ($rows | append {method: tar, pass: $pass, time: $t})
  }
}
for pass in [1 2] {
  let t = (per-member {|m| zip open --raw $z $m })
  $rows = ($rows | append {method: plugin, pass: $pass, time: $t})
}
let t = (timeit { zip open --raw $z ...$members | ignore })
$rows = ($rows | append {method: batch, pass: 1, time: $t})

$rows | insert "per member" {|r| $r.time / ($members | length) } | print
