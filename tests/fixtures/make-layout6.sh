#!/bin/sh
# Makes layout6.sql and layout6-tokens.json, next to this script: a database of layout 6 made by
# relevo 0.3.0 itself, through its CLI and HTTP API, for the migration test (tests/migrate.rs).
# Run it with a relevo 0.3.0 binary, built from its tag outside this checkout:
#
#   mkdir /tmp/v030 && git archive v0.3.0 | tar -x -C /tmp/v030
#   (cd /tmp/v030 && cargo build --release)
#   tests/fixtures/make-layout6.sh /tmp/v030/target/release/relevo
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
ana=$(token ana Ana "payments backend" "Claude Code")
bob=$(token bob Bob "infrastructure" "pi")
carl=$(token carl Carl "billing" "Hermes")

"$bin" --db "$db" serve --addr "127.0.0.1:$port" >"$dir/serve.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true; rm -rf "$dir"' EXIT
for _ in $(seq 50); do curl -sf "$url/health" >/dev/null && break; sleep 0.1; done

# req TOKEN SESSION CONTEXT METHOD PATH [BODY]
req() {
    curl -sf -X "$4" "$url$5" -H "authorization: Bearer $1" -H "relevo-session: $2" \
        -H "relevo-context: $3" -H 'content-type: application/json' ${6:+-d "$6"}
}
send() { req "$1" "$2" "$3" POST /api/send "$4" | jq -r .thread; }
inbox() { req "$1" "$2" "$3" GET /api/inbox >/dev/null; }

req "$bob" bob-term-02 "" GET /api/people >/dev/null
# Fetched by Bob, not answered: pending.
fetched=$(send "$ana" ana-cc-0001 pagos-api '{"to":["bob"],"text":"Fetched, not answered","key":"k-fetched"}')
inbox "$bob" bob-term-01 pagos-api
# Answered after reading.
replied=$(send "$ana" ana-cc-0001 pagos-api '{"to":["bob"],"text":"Replied?"}')
inbox "$bob" bob-term-01 pagos-api
send "$bob" bob-term-01 pagos-api "{\"thread\":\"$replied\",\"text\":\"Yes.\"}" >/dev/null
# Read and marked done.
done=$(send "$carl" carl-hermes1 "" '{"to":["bob"],"text":"FYI, deployed."}')
inbox "$bob" bob-term-01 pagos-api
send "$bob" bob-term-01 pagos-api "{\"thread\":\"$done\",\"done\":true}" >/dev/null
# A team question, answered by Carl and read by Ana: a global thread once migrated.
team=$(send "$ana" ana-cc-0001 pagos-api '{"to":["@team"],"text":"Who owns billing?"}')
inbox "$carl" carl-hermes1 ""
send "$carl" carl-hermes1 "" "{\"thread\":\"$team\",\"text\":\"Me.\"}" >/dev/null
inbox "$ana" ana-cc-0001 pagos-api
# A team question nobody has read.
unread_team=$(send "$carl" carl-hermes1 "" '{"to":["@team"],"text":"Anyone on call tonight?"}')
# A team question Ana closed after Bob read it, and a note Bob never fetched.
resolved=$(send "$ana" ana-cc-0001 pagos-api '{"to":["@team"],"text":"Is staging down?"}')
inbox "$bob" bob-term-01 pagos-api
send "$ana" ana-cc-0001 pagos-api "{\"thread\":\"$resolved\",\"text\":\"Back up, thanks.\",\"close\":true}" >/dev/null
# Read and claimed by Bob's first session, not answered: pending, and claimed.
claimed=$(send "$carl" carl-hermes1 "" '{"to":["bob"],"text":"Claim me"}')
inbox "$bob" bob-term-01 pagos-api
send "$bob" bob-term-01 pagos-api "{\"thread\":\"$claimed\",\"claim\":true}" >/dev/null
# Never fetched: pending.
unread=$(send "$ana" ana-cc-0001 pagos-api '{"to":["bob"],"text":"Never fetched."}')

kill $server
wait $server 2>/dev/null || true
test "$(sqlite3 "$db" 'PRAGMA user_version')" = 6
{
    sqlite3 "$db" .dump
    echo 'PRAGMA user_version=6;'
} >"$here/layout6.sql"
jq -n --arg ana "$ana" --arg bob "$bob" --arg carl "$carl" \
    --arg fetched "$fetched" --arg replied "$replied" --arg done "$done" --arg team "$team" \
    --arg unread_team "$unread_team" --arg resolved "$resolved" --arg claimed "$claimed" \
    --arg unread "$unread" \
    '{tokens: {ana: $ana, bob: $bob, carl: $carl},
      threads: {fetched: $fetched, replied: $replied, done: $done, team: $team,
                unread_team: $unread_team, resolved: $resolved, claimed: $claimed,
                unread: $unread}}' >"$here/layout6-tokens.json"
