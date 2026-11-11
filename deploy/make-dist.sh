#!/usr/bin/env bash
# Makes the dist of a release (docs/update.md): what the team's server hands out so that the
# clients update themselves. The release workflow runs it, and so do the tests.
#
# Usage: deploy/make-dist.sh --key <private.pem> [--pub <public.pem>] <dir>
#
#   <dir>         holds the release's binaries, relevo-<target> (one or more).
#   --key         the Ed25519 private key (PEM) that signs manifest.json.
#   --pub         a public key (PEM) the signature must check against: the one compiled into
#                 the clients (keys/release-signing-pub.pem). A key that does not match it
#                 stops here, rather than making a dist every client refuses.
#
# It writes into <dir>, for the version in Cargo.toml (the adapters must say the same one):
#
#   relevo-pi-<version>.tar.gz       the contents of adapters/pi
#   relevo-hermes-<version>.tar.gz   the contents of adapters/hermes/relevo
#   manifest.json                    every file above, with its size and SHA-256
#   manifest.sig                     the 64-byte Ed25519 signature of manifest.json
#   relevo-dist-<version>.tar.gz     all of it in one file, for deploy/install.sh --dist
#
# It needs bash, GNU tar, gzip, sha256sum and openssl.
set -euo pipefail

usage() {
    sed -n '5,10s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"
}

die() {
    printf 'make-dist.sh: %s\n' "$*" >&2
    exit 1
}

key=
pub=
dir=
while [ $# -gt 0 ]; do
    case "$1" in
        --key)
            [ $# -ge 2 ] || die "--key needs a file"
            key=$2
            shift
            ;;
        --pub)
            [ $# -ge 2 ] || die "--pub needs a file"
            pub=$2
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*)
            usage >&2
            die "unknown option: $1"
            ;;
        *)
            [ -z "$dir" ] || die "give one directory, not several"
            dir=$1
            ;;
    esac
    shift
done
if [ -z "$dir" ] || [ -z "$key" ]; then
    usage >&2
    exit 2
fi
[ -f "$key" ] || die "no key at $key"
[ -z "$pub" ] || [ -f "$pub" ] || die "no public key at $pub"
command -v openssl >/dev/null || die "openssl is needed to sign"

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -n 1)
[ -n "$version" ] || die "no version in $repo/Cargo.toml"
pi_version=$(sed -n 's/^ *"version": *"\(.*\)",\{0,1\}$/\1/p' "$repo/adapters/pi/package.json" | head -n 1)
hermes_version=$(sed -n 's/^version: *//p' "$repo/adapters/hermes/relevo/plugin.yaml" | head -n 1)
[ "$pi_version" = "$version" ] ||
    die "adapters/pi/package.json says $pi_version, Cargo.toml $version: bump them together"
[ "$hermes_version" = "$version" ] ||
    die "adapters/hermes/relevo/plugin.yaml says $hermes_version, Cargo.toml $version: bump them together"

# Before anything is written: the key must be the one the clients trust.
if [ -n "$pub" ]; then
    cmp -s <(openssl pkey -in "$key" -pubout) <(openssl pkey -pubin -in "$pub" -pubout) ||
        die "the key does not match $pub: the clients would refuse this dist"
fi

[ -d "$dir" ] || die "no directory $dir"
mapfile -t binaries < <(cd "$dir" && find . -maxdepth 1 -type f -name 'relevo-*' \
    ! -name '*.tar.gz' -printf '%f\n' | LC_ALL=C sort)
[ "${#binaries[@]}" -gt 0 ] || die "no relevo-<target> binaries in $dir"

# The same archive for the same sources: names sorted, no owners, one date.
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$repo" log -1 --format=%ct 2>/dev/null || echo 0)}
archive() {
    local src=$1 out=$2
    shift 2
    tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
        --exclude=node_modules --exclude=__pycache__ --exclude='*.pyc' "$@" \
        -C "$src" -cf - . | gzip -n -9 >"$out"
}
pi=relevo-pi-$version.tar.gz
hermes=relevo-hermes-$version.tar.gz
archive "$repo/adapters/pi" "$dir/$pi"
archive "$repo/adapters/hermes/relevo" "$dir/$hermes"

entry() {
    local name=$1 kind=$2 target=${3:-} size sha
    size=$(stat -c %s "$dir/$name")
    sha=$(sha256sum "$dir/$name" | cut -d ' ' -f 1)
    printf '    { "name": "%s", "kind": "%s", ' "$name" "$kind"
    [ -z "$target" ] || printf '"target": "%s", ' "$target"
    printf '"size": %s, "sha256": "%s" }' "$size" "$sha"
}
{
    printf '{\n  "version": "%s",\n  "files": [\n' "$version"
    for b in "${binaries[@]}"; do
        entry "$b" binary "${b#relevo-}"
        printf ',\n'
    done
    entry "$pi" pi
    printf ',\n'
    entry "$hermes" hermes
    printf '\n  ]\n}\n'
} >"$dir/manifest.json"

openssl pkeyutl -sign -rawin -inkey "$key" -in "$dir/manifest.json" -out "$dir/manifest.sig"
[ "$(stat -c %s "$dir/manifest.sig")" = 64 ] || die "the signature is not 64 bytes: not an Ed25519 key?"
verify() {
    openssl pkeyutl -verify -rawin -pubin -inkey "$1" -in "$dir/manifest.json" \
        -sigfile "$dir/manifest.sig" >/dev/null || die "the signature does not check"
}
if [ -n "$pub" ]; then
    verify "$pub"
else
    verify <(openssl pkey -in "$key" -pubout)
fi

files=(manifest.json manifest.sig "${binaries[@]}" "$pi" "$hermes")
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
    -C "$dir" -cf - "${files[@]}" | gzip -n -9 >"$dir/relevo-dist-$version.tar.gz"

printf 'dist %s in %s:\n' "$version" "$dir"
printf '  %s\n' "${files[@]}" "relevo-dist-$version.tar.gz"
