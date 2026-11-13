#!/bin/sh
# Makes layout7.sql and layout7-tokens.json, next to this script: a database of layout 7 made by
# relevo 0.4.4 itself, through its CLI and HTTP API, for the migration test (tests/migrate.rs).
# Run it with a relevo 0.4.4 binary, built from its tag outside this checkout:
#
#   mkdir /tmp/v044 && git archive v0.4.4 | tar -x -C /tmp/v044
#   (cd /tmp/v044 && cargo build --release)
#   tests/fixtures/make-layout7.sh /tmp/v044/target/release/relevo
#
# Needs curl, jq and sqlite3. The tokens are throwaway ones, for the test to act as each session.
set -eu
bin=$1
here=$(cd "$(dirname "$0")" && pwd)
dir=$(mktemp -d)
db=$dir/relevo.db
port=$((20000 + $(od -An -N2 -tu2 /dev/urandom) % 20000))
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

# req TOKEN SESSION CONTEXT METHOD PATH [BODY]
req() {
    curl -sf -X "$4" "$url$5" -H "authorization: Bearer $1" -H "relevo-session: $2" \
        -H "relevo-context: $3" -H 'relevo-mentions: 1' -H 'content-type: application/json' \
        ${6:+-d "$6"}
}
send() { req "$1" "$2" "$3" POST /api/send "$4" | jq -r .thread; }
inbox() { req "$1" "$2" "$3" GET /api/inbox >/dev/null; }

inbox "$ana" ana-cc-0001 pagos-api
inbox "$bob" bob-term-01 pagos-api
inbox "$carl" carl-hermes1 ""
# Fetched by Bob, not answered: pending.
fetched=$(send "$ana" ana-cc-0001 pagos-api '{"text":"@bob Fetched, not answered","key":"k-fetched"}')
inbox "$bob" bob-term-01 pagos-api
# Read and marked done.
done=$(send "$carl" carl-hermes1 "" '{"text":"@bob FYI, deployed."}')
inbox "$bob" bob-term-01 pagos-api
send "$bob" bob-term-01 pagos-api "{\"thread\":\"$done\",\"done\":true}" >/dev/null
# A direct thread Ana closed without reading Bob's answer, which he wrote holding it.
closed_direct=$(send "$ana" ana-cc-0001 pagos-api '{"text":"@bob Deploy tonight?"}')
inbox "$bob" bob-term-01 pagos-api
send "$bob" bob-term-01 pagos-api "{\"thread\":\"$closed_direct\",\"text\":\"Yes, at nine.\"}" >/dev/null
send "$ana" ana-cc-0001 pagos-api "{\"thread\":\"$closed_direct\",\"close\":true,\"key\":\"k-close\"}" >/dev/null
# A team thread naming Carl, read by Bob, which Ana closed with a last note: resolved for Bob,
# and Carl never fetched it.
closed_team=$(send "$ana" ana-cc-0001 pagos-api '{"text":"@backend @carl Is staging down?"}')
inbox "$bob" bob-term-01 pagos-api
send "$ana" ana-cc-0001 pagos-api "{\"thread\":\"$closed_team\",\"text\":\"Back up, thanks.\",\"close\":true}" >/dev/null
# A question on the global board, answered by Bob and closed by Carl, who asked.
closed_board=$(send "$carl" carl-hermes1 "" '{"text":"Anyone on call tonight?"}')
inbox "$bob" bob-term-01 pagos-api
send "$bob" bob-term-01 pagos-api "{\"thread\":\"$closed_board\",\"text\":\"Me.\"}" >/dev/null
inbox "$carl" carl-hermes1 ""
send "$carl" carl-hermes1 "" "{\"thread\":\"$closed_board\",\"close\":true}" >/dev/null
# Never fetched: pending.
unread=$(send "$ana" ana-cc-0001 pagos-api '{"text":"@bob Never fetched."}')

kill $server
wait $server 2>/dev/null || true
test "$(sqlite3 "$db" 'PRAGMA user_version')" = 7
{
    sqlite3 "$db" .dump
    echo 'PRAGMA user_version=7;'
} >"$here/layout7.sql"
jq -n --arg ana "$ana" --arg bob "$bob" --arg carl "$carl" \
    --arg fetched "$fetched" --arg done "$done" --arg closed_direct "$closed_direct" \
    --arg closed_team "$closed_team" --arg closed_board "$closed_board" --arg unread "$unread" \
    '{tokens: {ana: $ana, bob: $bob, carl: $carl},
      threads: {fetched: $fetched, done: $done, closed_direct: $closed_direct,
                closed_team: $closed_team, closed_board: $closed_board, unread: $unread}}' \
    >"$here/layout7-tokens.json"
