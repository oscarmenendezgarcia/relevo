#!/usr/bin/env bash
# Tests deploy/install.sh in a throwaway HOME under /tmp, with a stand-in for systemctl: it never
# touches the real ~/.local, ~/.config or the systemd user manager.
#
# Usage: deploy/test-install.sh <relevo-binary>
set -euo pipefail

[ $# -eq 1 ] || {
    echo "usage: $0 <relevo-binary>" >&2
    exit 2
}
binary=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
fake=$(mktemp -d /tmp/relevo-install-test.XXXXXX)
trap 'rm -rf "$fake"' EXIT
case "$fake" in
    /tmp/relevo-install-test.*) ;;
    *) exit 1 ;;
esac
home=$fake/home
mkdir "$home"

# Records its arguments; never calls the real one.
cat >"$fake/systemctl" <<EOF
#!/bin/sh
echo "\$*" >>"$fake/systemctl.log"
EOF
chmod +x "$fake/systemctl"

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

# Runs install.sh with the fake HOME and systemctl; its output goes to $fake/out.
install_() {
    env -u XDG_CONFIG_HOME HOME="$home" RELEVO_SYSTEMCTL="$fake/systemctl" \
        "$here/install.sh" "$@" >"$fake/out" 2>&1
}

contents() {
    (cd "$home" && find . -mindepth 1 | sort)
}

mode() {
    stat -c %a "$1"
}

expect_in_out() {
    grep -qF -- "$1" "$fake/out" || fail "expected '$1' in: $(cat "$fake/out")"
}

expect_not_in_out() {
    if grep -qF -- "$1" "$fake/out"; then fail "did not expect '$1' in: $(cat "$fake/out")"; fi
}

bin=$home/.local/bin/relevo
data=$home/.local/share/relevo
units=$home/.config/systemd/user

echo "dry run changes nothing"
install_ --dry-run --no-systemd "$binary"
[ -z "$(contents)" ] || fail "dry run wrote: $(contents)"
expect_in_out "would run: mkdir -p $home/.local/bin"
install_ --dry-run --enable "$binary"
[ -z "$(contents)" ] || fail "dry run wrote: $(contents)"
[ ! -e "$fake/systemctl.log" ] || fail "dry run called systemctl"
expect_in_out "would run: $fake/systemctl --user enable --now relevo.service relevo-backup.timer"

echo "--no-systemd installs the binary and private data directories only"
install_ --no-systemd "$binary"
cmp -s "$binary" "$bin" || fail "binary not installed"
[ "$(mode "$bin")" = 755 ] || fail "binary mode $(mode "$bin")"
[ "$(mode "$data")" = 700 ] || fail "data dir mode $(mode "$data")"
[ "$(mode "$data/backups")" = 700 ] || fail "backups dir mode $(mode "$data/backups")"
[ ! -e "$home/.config" ] || fail "--no-systemd wrote $home/.config"
[ ! -e "$home/.local/bin/.relevo.new" ] || fail "left a partial binary"
[ ! -e "$fake/systemctl.log" ] || fail "--no-systemd called systemctl"

echo "running it again changes nothing"
before=$(contents)
install_ --no-systemd "$binary"
[ "$(contents)" = "$before" ] || fail "second run changed files"
expect_not_in_out "installing $bin"
expect_in_out "up to date: $bin"

echo "a data directory made too open is made private again"
chmod 0755 "$data"
install_ --no-systemd "$binary"
[ "$(mode "$data")" = 700 ] || fail "data dir mode $(mode "$data")"

echo "without --enable, the units are installed and systemctl is not called"
install_ "$binary"
for unit in relevo.service relevo-backup.service relevo-backup.timer; do
    cmp -s "$here/systemd/$unit" "$units/$unit" || fail "$unit not installed"
done
[ ! -e "$fake/systemctl.log" ] || fail "called systemctl without --enable"
expect_in_out "systemctl --user enable --now relevo.service relevo-backup.timer"

if command -v systemd-analyze >/dev/null; then
    echo "the installed units verify against the installed binary"
    # Its own runtime directory too: a CI runner may have no user session.
    mkdir -m 0700 "$fake/run"
    HOME="$home" XDG_RUNTIME_DIR="$fake/run" systemd-analyze --user verify \
        "$units/relevo.service" "$units/relevo-backup.service" "$units/relevo-backup.timer"
    # Not installed by install.sh; it verifies only where cloudflared is installed.
    if command -v cloudflared >/dev/null; then
        HOME="$home" XDG_RUNTIME_DIR="$fake/run" systemd-analyze --user verify \
            "$here/proxy/relevo-tunnel.service"
    fi
fi

echo "--enable reloads and enables, and restarts only when something changed"
install_ --enable "$binary"
[ "$(cat "$fake/systemctl.log")" = "--user daemon-reload
--user enable --now relevo.service relevo-backup.timer" ] ||
    fail "systemctl calls: $(cat "$fake/systemctl.log")"
rm "$fake/systemctl.log"
# Another build: the same program with a byte more.
cp "$binary" "$fake/relevo-new"
printf '\0' >>"$fake/relevo-new"
install_ --enable "$fake/relevo-new"
cmp -s "$fake/relevo-new" "$bin" || fail "new binary not installed"
[ "$(cat "$fake/systemctl.log")" = "--user daemon-reload
--user try-restart relevo.service
--user enable --now relevo.service relevo-backup.timer" ] ||
    fail "systemctl calls: $(cat "$fake/systemctl.log")"
rm "$fake/systemctl.log"

echo "the backup the timer runs works with the installed binary"
RELEVO_DB=$data/relevo.db "$bin" invite ana --name Ana >/dev/null
RELEVO_DB=$data/relevo.db "$bin" backup --keep 14 "$data/backups" >/dev/null
[ "$(find "$data/backups" -name '????????-??????.db' | wc -l)" = 1 ] || fail "no backup written"

echo "a bare name is the file in the current directory, not one in PATH"
mkdir "$fake/cwd" "$fake/path"
cp "$binary" "$fake/path/relevo"
printf 'not a program' >"$fake/cwd/relevo"
if (cd "$fake/cwd" && PATH="$fake/path:$PATH" install_ --no-systemd relevo); then
    fail "checked the relevo in PATH and installed the text file here"
fi
expect_in_out "not executable"
cmp -s "$fake/relevo-new" "$bin" || fail "binary was replaced"
chmod +x "$fake/cwd/relevo"
if (cd "$fake/cwd" && PATH="$fake/path:$PATH" install_ --no-systemd relevo); then
    fail "checked the relevo in PATH and installed the text file here"
fi
expect_in_out "does not run here"
cp "$fake/relevo-new" "$fake/cwd/relevo-aarch64-unknown-linux-gnu"
(cd "$fake/cwd" && install_ --no-systemd relevo-aarch64-unknown-linux-gnu) ||
    fail "a bare name in the current directory: $(cat "$fake/out")"

echo "a binary that refuses the database replaces nothing"
if command -v python3 >/dev/null; then
    python3 -I - "$data/relevo.db" <<'EOF'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("PRAGMA user_version = 999")
db.commit()
EOF
    cp "$binary" "$fake/relevo-newer"
    printf '\0\0' >>"$fake/relevo-newer"
    if install_ --enable "$fake/relevo-newer"; then fail "installed over a refused database"; fi
    expect_in_out "nothing changed"
    expect_in_out "newer version of relevo"
    cmp -s "$fake/relevo-new" "$bin" || fail "binary was replaced"
    [ ! -e "$fake/systemctl.log" ] || fail "called systemctl after refusing"
else
    echo "  (skipped: no python3 to write the database)"
fi

echo "an upgrade from 0.2.x backs up first, never migrates, and serve migrates on its start"
if command -v python3 >/dev/null; then
    # A home of its own, with what 0.2.x left: its binary and a database of layout 5 in WAL mode.
    home=$fake/home5
    bin=$home/.local/bin/relevo
    data=$home/.local/share/relevo
    db=$data/relevo.db
    mkdir -p "$home/.local/bin" "$data"
    chmod 0700 "$data"
    printf 'relevo 0.2.1' >"$bin"
    chmod 0755 "$bin"
    python3 -I - "$db" "$here/../tests/fixtures/layout5.sql" <<'EOF2'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.executescript(open(sys.argv[2]).read())
db.execute("PRAGMA journal_mode=WAL")
db.close()
EOF2
    chmod 0600 "$db"
    version() {
        python3 -I -c 'import sqlite3, sys; print(sqlite3.connect(sys.argv[1]).execute("PRAGMA user_version").fetchone()[0])' "$1"
    }
    before=$(sha256sum <"$db")
    rm -f "$fake/systemctl.log"

    install_ --dry-run --enable "$binary"
    expect_in_out "migrates it in place"
    expect_in_out "would run: $binary --db $db backup $data/backups/before-upgrade-"
    [ ! -e "$data/backups" ] || fail "dry run made the backups directory"

    install_ --enable "$binary" || fail "upgrade: $(cat "$fake/out")"
    expect_in_out "the layout of relevo 0.1.0 to 0.2.x"
    cmp -s "$binary" "$bin" || fail "new binary not installed"
    # The probe and the backup only read it: the same bytes, no -wal or -shm left behind.
    [ "$(sha256sum <"$db")" = "$before" ] || fail "install.sh wrote the database"
    if [ -e "$db-wal" ] || [ -e "$db-shm" ]; then fail "install.sh left -wal or -shm files"; fi
    [ "$(version "$db")" = 5 ] || fail "install.sh migrated the database"
    copies=$(find "$data/backups" -name 'before-upgrade-????????-??????.db')
    [ "$(printf '%s\n' "$copies" | grep -c .)" = 1 ] || fail "backups: $copies"
    [ "$(mode "$copies")" = 600 ] || fail "backup mode $(mode "$copies")"
    [ "$(version "$copies")" = 5 ] || fail "the backup is not the old layout"
    grep -q try-restart "$fake/systemctl.log" || fail "no restart: $(cat "$fake/systemctl.log")"
    # The daily backup's --keep leaves it alone.
    "$bin" --db "$db" backup --keep 1 "$data/backups" >/dev/null
    [ -e "$copies" ] || fail "--keep removed the before-upgrade backup"

    # Until the server starts, admin commands refuse it, and say what to do.
    if out=$(RELEVO_DB=$db "$bin" people 2>&1); then fail "people ran on layout 5"; fi
    case $out in *"only \`relevo serve\`"*"relevo backup"*) ;; *) fail "people said: $out" ;; esac
    [ "$(sha256sum <"$db")" = "$before" ] || fail "people wrote the database"

    # The server migrates it when it starts; the team is still there, nothing to invite again.
    RELEVO_DB=$db "$bin" serve --addr 127.0.0.1:0 >"$fake/serve.log" 2>&1 &
    serve=$!
    for _ in $(seq 100); do
        [ "$(version "$db")" = 6 ] && break
        sleep 0.1
    done
    kill "$serve"
    wait "$serve" 2>/dev/null || true
    [ "$(version "$db")" = 6 ] || fail "serve did not migrate: $(cat "$fake/serve.log")"
    [ "$(RELEVO_DB=$db "$bin" people | cut -f1 | tr '\n' ' ')" = "ana bob carl " ] ||
        fail "people after the migration: $(RELEVO_DB=$db "$bin" people)"

    # Installing the same binary again: nothing to back up, nothing restarted.
    rm -f "$fake/systemctl.log"
    install_ --enable "$binary"
    expect_in_out "this version's layout"
    [ "$(find "$data/backups" -name 'before-upgrade-*' | wc -l)" = 1 ] || fail "backed up again"
    if grep -q try-restart "$fake/systemctl.log"; then fail "restarted an unchanged server"; fi
else
    echo "  (skipped: no python3 to write the database)"
fi

echo "bad arguments are refused"
if install_ --enable --no-systemd "$binary"; then fail "accepted --enable --no-systemd"; fi
if install_ "$fake/missing"; then fail "accepted a missing binary"; fi
printf 'not a program' >"$fake/text"
chmod +x "$fake/text"
if install_ --no-systemd "$fake/text"; then fail "accepted a binary that does not run"; fi
expect_in_out "does not run here"

echo "ok"
