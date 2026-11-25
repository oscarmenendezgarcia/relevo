#!/bin/sh
# Makes layout10.sql and layout10-tokens.json, next to this script: a database of layout 10, as
# relevo 0.8.1 made it, through its CLI and HTTP API, for the migration test (tests/migrate.rs):
# notes and thread titles in plain text, which layout 11 encrypts. Run it with the released
# binary of 0.8.1 for this machine:
#
#   mkdir /tmp/layout10 && curl -fL -o /tmp/layout10/relevo \
#     https://github.com/oscarmenendezgarcia/relevo/releases/download/v0.8.1/relevo-$(uname -m)-unknown-linux-gnu
#   chmod +x /tmp/layout10/relevo
#   tests/fixtures/make-layout10.sh /tmp/layout10/relevo
#
# Needs curl, jq and sqlite3. The tokens are throwaway ones, for the test to act as each session.
# It runs in a HOME of its own. PORT picks the port to serve on (default: a random one).
set -eu
bin=$1
here=$(cd "$(dirname "$0")" && pwd)
dir=$(mktemp -d)
db=$dir/relevo.db
port=${PORT:-$((20000 + $(od -An -N2 -tu2 /dev/urandom) % 20000))}
url=http://127.0.0.1:$port
export HOME=$dir/home
mkdir "$HOME"

token() { "$bin" --db "$db" invite "$1" --name "$2" --about "$3" --token --label "$4" | sed -n 2p; }
"$bin" --db "$db" serve --addr "127.0.0.1:$port" --no-release-notes >"$dir/serve.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true; rm -rf "$dir"' EXIT
for _ in $(seq 50); do curl -sf "$url/health" >/dev/null && break; sleep 0.1; done
ana=$(token ana Ana "payments backend" "ana's laptop")
bob=$(token bob Bob "infrastructure" "bob's desktop")
carl=$(token carl Carl "billing" "carl's server")
"$bin" --db "$db" team add backend --title Backend >/dev/null
"$bin" --db "$db" team join backend bob carl >/dev/null

# req TOKEN SESSION CONTEXT METHOD PATH [BODY], as a 0.8.1 client.
req() {
    case $2 in ana-*) agent='Claude Code' ;; bob-*) agent=pi ;; *) agent=Hermes ;; esac
    curl -sf -X "$4" "$url$5" -H "authorization: Bearer $1" -H "relevo-session: $2" \
        -H "relevo-context: $3" -H 'relevo-mentions: 1' -H 'relevo-client: mcp/0.8.1' \
        -H "relevo-agent: $agent" -H 'content-type: application/json' ${6:+-d "$6"}
}
send() { req "$1" "$2" "$3" POST /api/send "$4" | jq -r .thread; }
inbox() { req "$1" "$2" "$3" GET /api/inbox >/dev/null; }

inbox "$ana" ana-cc-0001 pagos-api
inbox "$bob" bob-term-01 pagos-api
inbox "$carl" carl-hermes1 ""
# A direct thread, answered.
direct=$(send "$ana" ana-cc-0001 pagos-api '{"text":"@bob Rotate the staging database password","key":"k-direct"}')
inbox "$bob" bob-term-01 pagos-api
send "$bob" bob-term-01 pagos-api "{\"thread\":\"$direct\",\"text\":\"Rotated, the vault has it.\"}" >/dev/null
# A question to the team, never fetched.
team=$(send "$ana" ana-cc-0001 pagos-api '{"text":"@backend Is the invoice queue draining?"}')
# On the global board, longer than the timeline shows of a note in a page (1,000 characters).
long=$(printf 'Lunch on Friday at the usual place. %.0s' $(seq 40))
board=$(send "$carl" carl-hermes1 "" "{\"text\":\"$long\"}")

kill $server
wait $server 2>/dev/null || true
test "$(sqlite3 "$db" 'PRAGMA user_version')" = 10
{
    sqlite3 "$db" .dump
    echo 'PRAGMA user_version=10;'
} >"$here/layout10.sql"
jq -n --arg ana "$ana" --arg bob "$bob" --arg carl "$carl" \
    --arg direct "$direct" --arg team "$team" --arg board "$board" \
    '{tokens: {ana: $ana, bob: $bob, carl: $carl},
      threads: {direct: $direct, team: $team, board: $board}}' \
    >"$here/layout10-tokens.json"
