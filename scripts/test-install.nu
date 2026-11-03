# Check install.nu without a network or a published release.
#
# The release workflow runs the installer for real on every packaged archive,
# but only along the happy path. This script covers what that path cannot
# reach: which releases are eligible, which checksum belongs to an asset, and
# what a bad archive leaves behind. The selection is fed crafted API answers;
# the archives are built here, in a throwaway directory, and installed by
# running install.nu itself.
#
#   nu scripts/test-install.nu
use std/assert

# The installer's selection helpers. `use` imports what it exports and runs
# nothing: its `main` stays out of scope.
use ../install.nu [candidates checksum-for]

def release [tag: string, names: list<string>, --prerelease, --draft] {
  {
    tag_name: $tag
    prerelease: $prerelease
    draft: $draft
    assets: ($names | each {|n| {name: $n, browser_download_url: $"https://example.invalid/($tag)/($n)"}})
  }
}

def main [] {
  let archive = "nu_plugin_zip-nu0.115.1-x86_64-unknown-linux-musl.tar.gz"
  let releases = [
    (release "v0.3.0-beta.1+nu-0.115.1" [$archive $"($archive).sha256"] --prerelease)
    (release "v0.2.1+nu-0.115.1" [$archive $"($archive).sha256"] --draft)
    (release "v0.2.0+nu-0.115.1" [$archive])
    (release "v0.1.0+nu-0.115.1" [$archive $"($archive).sha256"])
  ]

  # A pre-release or a draft is never picked unless asked for.
  assert equal (candidates $releases | get tag | uniq) [v0.2.0+nu-0.115.1 v0.1.0+nu-0.115.1]
  assert equal (candidates $releases --prerelease | get tag | uniq) [v0.3.0-beta.1+nu-0.115.1 v0.2.0+nu-0.115.1 v0.1.0+nu-0.115.1]
  # The checksum comes from the asset's own release, and its absence is fatal:
  # archive names repeat across releases, so another release's .sha256 would
  # check the wrong file.
  let all = (candidates $releases)
  let old = ($all | where tag == v0.1.0+nu-0.115.1 and name == $archive | first)
  assert equal (checksum-for $all $old) $"https://example.invalid/v0.1.0+nu-0.115.1/($archive).sha256"
  let bare = ($all | where tag == v0.2.0+nu-0.115.1 | first)
  assert (try { checksum-for $all $bare; false } catch { true }) "a release without a checksum must be refused"
  # Local archives, through the real entry point: a good one installs, a bad
  # one fails and leaves no staging directory behind.
  let work = (mktemp --directory)
  let exe = if $nu.os-info.name == "windows" { "nu_plugin_zip.exe" } else { "nu_plugin_zip" }
  let install = {|archive: string, dest: string|
    ^$nu.current-exe --no-config-file ($env.FILE_PWD | path join .. install.nu) --archive $archive --dir $dest | complete
  }
  let staged = {|| ls $nu.temp-dir | where name =~ 'nu_plugin_zip-' | get name }
  let before = (do $staged)

  mkdir ($work | path join good)
  "#!/bin/sh\n" | save ($work | path join good $exe)
  tar -czf ($work | path join good.tar.gz) -C ($work | path join good) $exe
  let run = (do $install ($work | path join good.tar.gz) ($work | path join dest))
  assert equal $run.exit_code 0 $run.stderr
  assert equal (($work | path join dest $exe) | path type) file
  "not an archive" | save ($work | path join bad.tar.gz)
  let run = (do $install ($work | path join bad.tar.gz) ($work | path join dest3))
  assert ($run.exit_code != 0) "a corrupt archive must fail"

  assert equal (do $staged) $before "a failed install must not leave a staging directory behind"
  rm -r -f $work
  print "install.nu ok"
}
