#!/usr/bin/env bash
# Installs a Relevo server for the current user, without root:
#
#   the binary          ~/.local/bin/relevo
#   the data directory  ~/.local/share/relevo (0700), with backups/ (0700)
#   the systemd units   ~/.config/systemd/user/relevo.service, relevo-backup.{service,timer}
#   the dist            ~/.local/share/relevo/dist: the clients the server hands out
#
# Usage: deploy/install.sh [--enable] [--dry-run] [--no-systemd] [--dist <tarball>] <relevo-binary>
#
#   --enable      also run `systemctl --user daemon-reload`, restart the server if it was running
#                 an older binary or unit, and `systemctl --user enable --now` the server and the
#                 daily backup timer.
#   --dry-run     say what would change, change nothing.
#   --no-systemd  install the binary and the data directory only.
#   --dist        the release's relevo-dist-<version>.tar.gz, for the clients to update from
#                 (docs/update.md). Without it, the one of the binary's version next to the
#                 binary, if there is one.
#
# Running it again is safe: what is already in place is left alone. Before replacing anything it
# checks that the new binary runs here and accepts the existing database, as it is or once
# `relevo serve` migrates it (`relevo check-db`, which only reads it: this script never migrates
# a database). When the binary changes, it first copies the database into
# backups/before-upgrade-<UTC time>.db with `relevo backup`. See "Upgrades" in docs/install.md.
# It never uses sudo.
set -euo pipefail

usage() {
    sed -n '9,18s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"
}

die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

enable=0
dry_run=0
systemd=1
binary=
dist=
while [ $# -gt 0 ]; do
    case "$1" in
        --enable) enable=1 ;;
        --dist)
            [ $# -ge 2 ] || die "--dist needs a file"
            dist=$2
            shift
            ;;
        --dry-run) dry_run=1 ;;
        --no-systemd) systemd=0 ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*)
            usage >&2
            die "unknown option: $1"
            ;;
        *)
            [ -z "$binary" ] || die "give one binary, not several"
            binary=$1
            ;;
    esac
    shift
done
[ -n "$binary" ] || {
    usage >&2
    exit 2
}
[ "$enable" = 0 ] || [ "$systemd" = 1 ] || die "--enable and --no-systemd exclude each other"
# A bare name is the file here, as `install` copies it: run that one, not one found in PATH.
case $binary in
    */*) ;;
    *) binary=./$binary ;;
esac
[ -n "${HOME:-}" ] || die "HOME is not set"

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
bin_dir=$HOME/.local/bin
data_dir=$HOME/.local/share/relevo
backup_dir=$data_dir/backups
dist_dir=$data_dir/dist
db=$data_dir/relevo.db
unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
units=(relevo.service relevo-backup.service relevo-backup.timer)
# Tests point this at a stand-in; nothing else should.
systemctl=${RELEVO_SYSTEMCTL:-systemctl}

say() {
    printf '%s\n' "$*"
}

# Runs a command, or only shows it with --dry-run.
run() {
    if [ "$dry_run" = 1 ]; then
        printf 'would run:'
        printf ' %q' "$@"
        printf '\n'
    else
        "$@"
    fi
}

# The binary: it must run on this machine (right architecture, right C library).
[ -f "$binary" ] || die "no file at $binary"
[ -x "$binary" ] || die "$binary is not executable: chmod +x it first"
version=$("$binary" --version 2>&1) || die "$binary does not run here: $version"
say "$binary: $version"

# A new binary may refuse a database made by another version: find out before replacing anything.
# Only reading it: an older layout is migrated by `relevo serve` when it starts, never here.
if [ -e "$db" ]; then
    if ! out=$("$binary" --db "$db" check-db 2>&1); then
        die "nothing changed: this binary does not accept $db:
  $out
See \"Upgrades\" in docs/install.md."
    fi
    say "$out"
fi

# The dist, checked before anything changes: unpacked aside, of the binary's version, every file
# as its manifest says. The clients check its signature; this only catches a wrong or broken file.
if [ -z "$dist" ]; then
    beside=$(dirname "$binary")/relevo-dist-${version#relevo }.tar.gz
    if [ -f "$beside" ]; then dist=$beside; fi
fi
if [ -n "$dist" ]; then
    [ -f "$dist" ] || die "no file at $dist"
    staged=$(mktemp -d "${TMPDIR:-/tmp}/relevo-dist.XXXXXX")
    trap 'rm -rf "$staged"' EXIT
    tar -xzf "$dist" -C "$staged" --no-same-owner 2>/dev/null ||
        die "nothing changed: $dist is not a .tar.gz"
    if [ ! -f "$staged/manifest.json" ] || [ ! -f "$staged/manifest.sig" ]; then
        die "nothing changed: $dist has no manifest.json and manifest.sig: not a relevo dist"
    fi
    dist_version=$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' "$staged/manifest.json" | head -n 1)
    [ "relevo $dist_version" = "$version" ] ||
        die "nothing changed: $dist is the dist of ${dist_version:-no version}, and the binary is $version: give the dist of the same release"
    sums=$(sed -n 's/.*"name": *"\([^"/]*\)".*"sha256": *"\([0-9a-f]\{64\}\)".*/\2  \1/p' "$staged/manifest.json")
    if [ -z "$sums" ] || ! (cd "$staged" && printf '%s\n' "$sums" | sha256sum --check --quiet --strict >/dev/null 2>&1); then
        die "nothing changed: the files in $dist do not match its manifest"
    fi
    say "$dist: the dist of $dist_version"
fi

changed_binary=1
if [ -f "$bin_dir/relevo" ] && cmp -s "$binary" "$bin_dir/relevo"; then
    say "up to date: $bin_dir/relevo"
    changed_binary=0
fi

# Holds every note: the server's user only.
private_dir() {
    if [ ! -d "$1" ]; then
        say "creating $1 (0700)"
        run mkdir -p "$1"
        run chmod 0700 "$1"
    elif [ "$(stat -c %a "$1")" != 700 ]; then
        say "making $1 private (0700)"
        run chmod 0700 "$1"
    else
        say "up to date: $1"
    fi
}
private_dir "$data_dir"
private_dir "$backup_dir"

if [ "$changed_binary" = 1 ]; then
    # Before the server can run the new binary, which may migrate the database: a copy of it as
    # it is, readable by the old version. `--keep` never removes it (it is not named like the
    # daily ones).
    if [ -e "$db" ]; then
        copy=$backup_dir/before-upgrade-$(date -u +%Y%m%d-%H%M%S).db
        say "backing up $db to $copy"
        run "$binary" --db "$db" backup "$copy" ||
            die "nothing installed: the backup of $db failed"
    fi
    say "installing $bin_dir/relevo"
    run mkdir -p "$bin_dir"
    # Into place with a rename, so a running server keeps its old file until it restarts.
    run install -m 0755 "$binary" "$bin_dir/.relevo.new"
    run mv -f "$bin_dir/.relevo.new" "$bin_dir/relevo"
fi

# Into place with one step: `dist` is a link to a directory of its own, and a new link is
# renamed over it, so the server hands out the old dist or the new one, never half of each.
if [ -n "$dist" ]; then
    if [ -f "$dist_dir/manifest.json" ] && cmp -s "$staged/manifest.json" "$dist_dir/manifest.json" &&
        cmp -s "$staged/manifest.sig" "$dist_dir/manifest.sig"; then
        say "up to date: $dist_dir ($dist_version)"
    else
        say "installing the dist $dist_version into $dist_dir"
        target=.dist-$dist_version-$(date -u +%Y%m%d-%H%M%S)-$$
        previous=
        if [ -L "$dist_dir" ]; then
            previous=$(readlink "$dist_dir")
            # Only a directory this script made is ever removed.
            case $previous in
                .dist-*/* | */*) previous= ;;
                .dist-*) ;;
                *) previous= ;;
            esac
        elif [ -e "$dist_dir" ]; then
            # A directory put there by hand: moved aside first, the one step that is not atomic.
            previous=.dist-old-$(date -u +%Y%m%d-%H%M%S)-$$
            run mv "$dist_dir" "$data_dir/$previous"
        fi
        run mv "$staged" "$data_dir/$target"
        run ln -sfn "$target" "$data_dir/.dist.link"
        run mv -T "$data_dir/.dist.link" "$dist_dir"
        if [ -n "$previous" ] && [ "$previous" != "$target" ]; then
            run rm -rf "${data_dir:?}/$previous"
        fi
    fi
elif [ -e "$dist_dir" ]; then
    say "no dist given: the clients in $dist_dir stay as they are"
else
    say "no dist given or next to the binary: this server hands out no clients to update from"
    say "(the release's relevo-dist-<version>.tar.gz; see docs/update.md)"
fi

changed_units=0
if [ "$systemd" = 1 ]; then
    for unit in "${units[@]}"; do
        [ -f "$here/systemd/$unit" ] || die "missing $here/systemd/$unit"
        if [ -f "$unit_dir/$unit" ] && cmp -s "$here/systemd/$unit" "$unit_dir/$unit"; then
            say "up to date: $unit_dir/$unit"
        else
            say "installing $unit_dir/$unit"
            run mkdir -p "$unit_dir"
            run install -m 0644 "$here/systemd/$unit" "$unit_dir/$unit"
            changed_units=1
        fi
    done
fi

linger_note() {
    if [ ! -e "/var/lib/systemd/linger/$(id -un)" ]; then
        say ""
        say "Relevo runs while $(id -un) is logged in. To start it at boot and keep it running"
        say "after logout, someone with admin rights runs (the owner's call):"
        say "  sudo loginctl enable-linger $(id -un)"
    fi
}

if [ "$enable" = 1 ]; then
    command -v "$systemctl" >/dev/null || die "systemctl not found; use --no-systemd"
    run "$systemctl" --user daemon-reload
    if [ "$changed_binary" = 1 ] || [ "$changed_units" = 1 ]; then
        # Only if it was running: then it runs the old binary or unit.
        run "$systemctl" --user try-restart relevo.service
    fi
    run "$systemctl" --user enable --now relevo.service relevo-backup.timer
    say ""
    say "Relevo listens on 127.0.0.1:7878. Check it: systemctl --user status relevo"
    say "Its log: journalctl --user -u relevo"
    linger_note
elif [ "$systemd" = 1 ]; then
    say ""
    say "To start it now and at every login:"
    say "  systemctl --user daemon-reload"
    say "  systemctl --user enable --now relevo.service relevo-backup.timer"
    say "(or run this again with --enable)"
    linger_note
else
    say ""
    say "Run the server yourself: RELEVO_DB=$db $bin_dir/relevo serve"
fi

say ""
say "Admin commands (invite, people, backup...) must use the same database:"
say "  export RELEVO_DB=$db    # add it to ~/.profile"
case ":${PATH:-}:" in
    *":$bin_dir:"*) ;;
    *) say "  export PATH=\"$bin_dir:\$PATH\"    # $bin_dir is not in PATH" ;;
esac
[ "$dry_run" = 0 ] || say "(dry run: nothing was changed)"
