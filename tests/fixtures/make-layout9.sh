#!/bin/sh
# Makes layout9.sql and layout9-tokens.json, next to this script: a database of layout 9, as
# relevo 0.5.0 made it, through its CLI and HTTP API, for the migration test (tests/migrate.rs).
# Run it with relevo 0.5.0, built outside this checkout:
#
#   mkdir /tmp/layout9 && git archive v0.5.0 | tar -x -C /tmp/layout9
#   (cd /tmp/layout9 && cargo build)
#   tests/fixtures/make-layout9.sh /tmp/layout9/target/debug/relevo
#
# Needs curl, jq and sqlite3. The tokens are throwaway ones, for the test to act as each session.
# PORT picks the port to serve on (default: a random one).
set -eu
bin=$1
here=$(cd "$(dirname "$0")" && pwd)
dir=$(mktemp -d)
db=$dir/relevo.db
port=${PORT:-$((20000 + $(od -An -N2 -tu2 /dev/urandom) % 20000))}
url=http://127.0.0.1:$port

token() { "$bin" --db "$db" invite "$1" --name "$2" --about "$3" --label "$4" | sed -n 2p; }
"$bin" --db "$db" serve --addr "127.0.0.1:$port" >"$dir/serve.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true; rm -rf "$dir"' EXIT
for _ in $(seq 50); do curl -sf "$url/health" >/dev/null && break; sleep 0.1; done
ana=$(token ana Ana "payments backend" "Claude Code")
bob=$(token bob Bob "infrastructure" "pi")
carl=$(token carl Carl "billing" "Hermes")
"$bin" --db "$db" team add backend --title Backend >/dev/null
"$bin" --db "$db" team join backend bob carl >/dev/null

# req TOKEN SESSION CONTEXT METHOD PATH [BODY], as a 0.5.0 client: it says its client, not its
# agent (no Relevo-Agent yet).
req() {
    case $2 in ana-*) client=mcp/0.5.0 ;; bob-*) client=pi/0.5.0 ;; *) client=hermes/0.5.0 ;; esac
    curl -sf -X "$4" "$url$5" -H "authorization: Bearer $1" -H "relevo-session: $2" \
        -H "relevo-context: $3" -H 'relevo-mentions: 1' -H "relevo-client: $client" \
        -H 'content-type: application/json' ${6:+-d "$6"}
}
send() { req "$1" "$2" "$3" POST /api/send "$4" | jq -r .thread; }
inbox() { req "$1" "$2" "$3" GET /api/inbox >/dev/null; }

inbox "$ana" ana-cc-0001 pagos-api
inbox "$bob" bob-term-01 pagos-api
inbox "$carl" carl-hermes1 ""
# Fetched by Bob, not answered: pending.
fetched=$(send "$ana" ana-cc-0001 pagos-api '{"text":"@bob Fetched, not answered","key":"k-fetched"}')
inbox "$bob" bob-term-01 pagos-api
# Answered by Bob.
answered=$(send "$carl" carl-hermes1 "" '{"text":"@bob Deploy tonight?"}')
inbox "$bob" bob-term-01 pagos-api
send "$bob" bob-term-01 pagos-api "{\"thread\":\"$answered\",\"text\":\"Yes, at nine.\"}" >/dev/null
# A question to the team.
team=$(send "$ana" ana-cc-0001 pagos-api '{"text":"@backend Is staging down?"}')
# Never fetched: pending.
unread=$(send "$ana" ana-cc-0001 pagos-api '{"text":"@bob Never fetched."}')

kill $server
wait $server 2>/dev/null || true
test "$(sqlite3 "$db" 'PRAGMA user_version')" = 9
{
    sqlite3 "$db" .dump
    echo 'PRAGMA user_version=9;'
} >"$here/layout9.sql"
jq -n --arg ana "$ana" --arg bob "$bob" --arg carl "$carl" \
    --arg fetched "$fetched" --arg answered "$answered" --arg team "$team" --arg unread "$unread" \
    '{tokens: {ana: $ana, bob: $bob, carl: $carl},
      threads: {fetched: $fetched, answered: $answered, team: $team, unread: $unread}}' \
    >"$here/layout9-tokens.json"
