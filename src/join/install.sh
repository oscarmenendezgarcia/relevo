#!/bin/sh
# Joins a Relevo server with an invitation: installs relevo in ~/.local/bin, then runs
# `relevo connect --join`, which trades the invitation for a token and saves it. The server
# serves this script for one invitation (docs/install.md, part 7); read it before running it.
#
#   curl -sS <the invitation link>/install | sh [-s -- <options for relevo connect>]
#
# It never uses sudo, and never shows the token.
set -eu

# Filled in by the server.
url=@URL@
code=@CODE@
# The pin of the server's key, when it speaks TLS with a certificate of its own (curl
# --pinnedpubkey), and that certificate's SHA-256 (relevo connect --fingerprint).
pin=@PIN@
fingerprint=@FINGERPRINT@
# The binaries the server hands out: a target and its SHA-256 per line.
sums=@SUMS@

fail() {
    printf 'relevo: %s\n' "$*" >&2
    exit 1
}

# fetch <url> <file>: trusting the server by its pin when it has one.
fetch() {
    if [ -n "$pin" ]; then
        curl -sSf --pinnedpubkey "sha256//$pin" -k -o "$2" "$1"
    else
        curl -sSf -o "$2" "$1"
    fi
}

main() {
    case "$(uname -s)/$(uname -m)" in
        Linux/x86_64 | Linux/amd64) target=x86_64-unknown-linux-gnu what='Linux (x86_64)' ;;
        Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-gnu what='Linux (ARM64)' ;;
        Darwin/arm64) target=aarch64-apple-darwin what='macOS (Apple Silicon)' ;;
        *) fail "relevo runs on Linux (x86_64, ARM64) and macOS (Apple Silicon), not on $(uname -s) $(uname -m)" ;;
    esac
    sum=$(printf '%s\n' "$sums" | sed -n "s/^$target //p")
    [ -n "$sum" ] || fail "the server hands out no relevo for $what: ask its owner"

    dir="$HOME/.local/bin"
    mkdir -p "$dir"
    tmp=$(mktemp "$dir/.relevo.XXXXXX")
    trap 'rm -f "$tmp"' EXIT
    trap 'exit 1' INT TERM HUP
    echo "Downloading relevo for $what from $url"
    fetch "$url/join/$code/relevo?target=$target" "$tmp" || fail "could not download relevo"
    if command -v sha256sum >/dev/null 2>&1; then
        got=$(sha256sum "$tmp" | cut -d ' ' -f 1)
    elif command -v shasum >/dev/null 2>&1; then
        got=$(shasum -a 256 "$tmp" | cut -d ' ' -f 1)
    else
        fail "there is neither sha256sum nor shasum here to check the download"
    fi
    [ "$got" = "$sum" ] || fail "the download is not the file the server lists: nothing installed"
    chmod 755 "$tmp"
    mv -f "$tmp" "$dir/relevo"
    trap - EXIT INT TERM HUP
    echo "Installed relevo in $dir/relevo"
    case ":$PATH:" in
        *":$dir:"*) ;;
        *) echo "$dir is not in your PATH: add it in your shell's profile, as export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
    esac

    # The server's fingerprint, unless one was given.
    for arg in "$@"; do
        case $arg in --fingerprint | --fingerprint=*) fingerprint= ;; esac
    done
    set -- connect --join "$code" "$@" "$url"
    if [ -n "$fingerprint" ]; then
        set -- "$@" --fingerprint "$fingerprint"
    fi
    # Its questions go to the terminal of a person running this (not of an agent, whose output
    # is no terminal): standard input is this script.
    if [ -t 1 ] && (: </dev/tty) 2>/dev/null; then
        exec "$dir/relevo" "$@" </dev/tty
    fi
    exec "$dir/relevo" "$@" </dev/null
}

main "$@"
