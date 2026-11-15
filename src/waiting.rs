//! `/relevo` (docs/agents.md, "The /relevo command"): the threads waiting on the person, as the
//! numbered list `relevo mcp`'s `waiting` tool shows, and the lines its actions answer with.
//! Pure text from the timeline's answers (`docs/protocol.md`, "timeline"): nothing here talks to
//! the server.

use serde_json::{Map, Value};

use crate::render::neutralise;

/// The most threads the list shows; the rest are counted.
pub const MAX_SHOWN: usize = 10;
/// How much of a note's first line the list shows.
const LINE_CHARS: usize = 100;
/// How much of a thread's first line names it in an action's answer.
const SUBJECT_CHARS: usize = 40;

/// A thread of the list, as its number refers to it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    /// The name of whoever opened it ("You" for the person).
    pub who: String,
    /// Its first line, short.
    pub subject: String,
    /// The latest note the list showed: an action refuses when a newer one came since.
    pub last_note: String,
}

impl Entry {
    /// `thread 1 (Ana · onboarding copy)`.
    pub fn named(&self, n: usize) -> String {
        format!("thread {n} ({} · {})", self.who, self.subject)
    }
}

/// The last list shown in this process: whom it was for, the names it knew, and its threads.
#[derive(Clone, Debug, Default)]
pub struct Listed {
    pub you: String,
    pub names: Map<String, Value>,
    pub entries: Vec<Entry>,
}

impl Listed {
    /// The name to show for `handle`: "You", their name, or the handle.
    pub fn name(&self, handle: &str) -> String {
        name(&self.names, &self.you, handle)
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn name(names: &Map<String, Value>, you: &str, handle: &str) -> String {
    if handle == you {
        return "You".into();
    }
    names
        .get(handle)
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(handle)
        .to_string()
}

/// `text` cut to `max` characters, at a word when one ends in the second half, with `…` when
/// cut.
fn cut(text: &str, max: usize) -> String {
    let Some((at, _)) = text.char_indices().nth(max) else {
        return text.to_string();
    };
    let head = &text[..at];
    let at = match head.rfind(char::is_whitespace) {
        Some(space) if head[..space].chars().count() >= max / 2 => space,
        _ => at,
    };
    format!("{}…", text[..at].trim_end())
}

/// The first line of `text` that is not blank, without control characters (a terminal shows
/// what the model prints), and, with `mentions`, without the `@` names it starts with: the list
/// is for the person, who knows a note names them.
pub fn first_line(text: &str, mentions: bool, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut line: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .trim_start_matches('#')
        .trim()
        .to_string();
    if mentions {
        let mut rest = line.as_str();
        while let Some(after) = rest.strip_prefix('@')
            && after.starts_with(|c: char| c.is_alphanumeric())
        {
            rest = after
                .split_once(char::is_whitespace)
                .map_or("", |(_, r)| r)
                .trim_start();
        }
        if !rest.is_empty() {
            line = rest.to_string();
        }
    }
    neutralise(&cut(&line, max))
}

/// `2026-11-08T10:47:00Z` (as the server writes times) in unix seconds.
pub fn unix(at: &str) -> Option<i64> {
    let (date, rest) = at.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let rest = rest.trim_end_matches('Z');
    let rest = rest.split(['.', '+']).next()?;
    let mut t = rest.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (h, min, sec) = (t.next()??, t.next()??, t.next()??);
    let month = time::Month::try_from(u8::try_from(m).ok()?).ok()?;
    let date =
        time::Date::from_calendar_date(i32::try_from(y).ok()?, month, u8::try_from(day).ok()?)
            .ok()?;
    let time = time::Time::from_hms(
        u8::try_from(h).ok()?,
        u8::try_from(min).ok()?,
        u8::try_from(sec).ok()?,
    )
    .ok()?;
    Some(
        time::PrimitiveDateTime::new(date, time)
            .assume_utc()
            .unix_timestamp(),
    )
}

/// How long ago, short: `now`, `5m`, `2h`, `3d`.
pub fn age(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// The offset of this machine's local time from UTC at `at`, in seconds (from `TZ` and the
/// system's zone files, as `date` reads them); 0 when it cannot be told.
pub fn local_offset(at: i64) -> i64 {
    unsafe extern "C" {
        /// POSIX; the libc crate does not declare it.
        fn tzset();
    }
    // SAFETY: `tzset` reads the environment; `localtime_r` writes only the `tm` given.
    unsafe {
        tzset();
        let t = at as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    }
}

/// When the local day of `now` began, in unix seconds, for an offset of `offset` seconds.
pub fn start_of_day(now: i64, offset: i64) -> i64 {
    now - (now + offset).rem_euclid(86_400)
}

/// `11:20` in local time when it is the day of `now`, else `11:20 on 2026-11-14`.
pub fn clock(at: i64, now: i64, offset: i64) -> String {
    let local = at + offset;
    let hm = format!(
        "{:02}:{:02}",
        local.rem_euclid(86_400) / 3600,
        local.rem_euclid(3600) / 60
    );
    if start_of_day(at, offset) == start_of_day(now, offset) {
        return hm;
    }
    match time::OffsetDateTime::from_unix_timestamp(local) {
        Ok(t) => format!("{hm} on {}", t.date()),
        Err(_) => hm,
    }
}

/// The host (and port) of a server URL: `relevo.office:8443`.
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// The kind of a thread as the list says it.
fn kind(t: &Value) -> String {
    match s(&t["kind"]) {
        "team" => {
            let teams: Vec<String> = t["teams"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|t| format!("@{t}"))
                .collect();
            if teams.is_empty() {
                "team".into()
            } else {
                format!("team {}", teams.join(" "))
            }
        }
        "global" => "board".into(),
        other => other.into(),
    }
}

/// The list of threads waiting on the person. `threads` are the timeline's, oldest first;
/// `total` is how many wait (`counts.waiting`); `handled` the threads handled today, said only
/// when nothing waits. Returns the text and what its numbers refer to.
pub fn list(
    you: &str,
    names: &Map<String, Value>,
    threads: &[Value],
    total: usize,
    handled: Option<i64>,
    now: i64,
) -> (String, Vec<Entry>) {
    if threads.is_empty() {
        let mut out = "✓ Nothing is waiting on you.".to_string();
        if let Some(n) = handled.filter(|n| *n > 0) {
            out.push_str(&format!("  Your agents handled {n} today."));
        }
        return (out, vec![]);
    }
    let total = total.max(threads.len());
    let mut out = format!("Waiting on you · {total}\n");
    let mut entries = vec![];
    for (i, t) in threads.iter().take(MAX_SHOWN).enumerate() {
        let n = i + 1;
        let first = &t["first_note"];
        let last = &t["last_note"];
        let opener = if first["author"].is_string() {
            s(&first["author"])
        } else {
            s(&t["from"])
        };
        let who = name(names, you, opener);
        let latest = if last.is_object() { last } else { first };
        let at = unix(s(&latest["created_at"])).unwrap_or(now);
        let mut head = format!("{n:>2}  {who} · {} · {}", kind(t), age(now - at));
        let replies = t["notes"].as_i64().unwrap_or(1) - 1;
        if replies > 1 {
            head.push_str(&format!(" · {replies} replies"));
        }
        if s(&t["kind"]) == "global" {
            head.push_str(" · mentioned you");
        }
        out.push_str(&head);
        out.push('\n');
        let text = if first["text"].is_string() {
            s(&first["text"])
        } else {
            s(&t["title"])
        };
        out.push_str(&format!("    {}\n", first_line(text, true, LINE_CHARS)));
        if last.is_object() {
            let author = s(&last["author"]);
            let by = if author == opener {
                String::new()
            } else {
                format!("{}: ", name(names, you, author))
            };
            out.push_str(&format!(
                "    └ {by}{}\n",
                first_line(s(&last["text"]), false, LINE_CHARS)
            ));
        }
        entries.push(Entry {
            id: s(&t["id"]).to_string(),
            who,
            subject: first_line(text, true, SUBJECT_CHARS),
            last_note: s(&latest["id"]).to_string(),
        });
    }
    if total > MAX_SHOWN {
        out.push_str(&format!("+{} more · relevo web\n", total - MAX_SHOWN));
    }
    let (a, b, c) = (1, 2.min(entries.len()), 3.min(entries.len()));
    out.push_str(&format!(
        "/relevo {a} <your answer> · /relevo {b} to open it · /relevo seen {c}"
    ));
    (out, entries)
}

/// What is left waiting after an action: `2 left`, or nothing.
pub fn left(waiting: Option<i64>) -> String {
    match waiting {
        Some(0) => "Nothing else is waiting on you.".into(),
        Some(n) => format!("{n} left"),
        None => String::new(),
    }
}

/// The person's entry in a thread's `delivery` (`GET /api/threads/:id`).
fn delivery<'a>(thread: &'a Value, you: &str) -> Option<&'a Value> {
    thread["delivery"]
        .as_array()?
        .iter()
        .find(|d| s(&d["handle"]) == you)
}

/// Why an action on list number `n` must not go ahead, from the thread as it is now
/// (`GET /api/threads/:id`): it no longer waits on the person (another of their sessions handled
/// it), or a note came after the one the list showed, which the person has not seen. `None`
/// when it may.
pub fn refusal(
    n: usize,
    e: &Entry,
    thread: &Value,
    listed: &Listed,
    now: i64,
    offset: impl Fn(i64) -> i64,
) -> Option<String> {
    let who = &e.who;
    if thread["pending"] != true {
        let d = delivery(thread, &listed.you);
        let at = d.and_then(|d| unix(s(&d["handled_at"])));
        let by = d.map(|d| &d["handled_by"]).filter(|b| b.is_object());
        let mut out = format!("Thread {n} ({who}) ");
        match (at, by) {
            (Some(at), by) => {
                out.push_str("was handled");
                if let Some(by) = by {
                    out.push_str(&format!(" by your {}", s(&by["label"])));
                    if let Some(context) = by["context"].as_str() {
                        out.push_str(&format!(" in {context}"));
                    }
                }
                out.push_str(&format!(" at {}", clock(at, now, offset(at))));
            }
            (None, _) => out.push_str("is no longer waiting on you"),
        }
        out.push_str(" · nothing sent");
        return Some(out);
    }
    let latest = thread["notes"].as_array().and_then(|n| n.last())?;
    if s(&latest["id"]) != e.last_note {
        return Some(format!(
            "Thread {n} ({who}) has a new note since your list, by {}: \"{}\" · nothing sent. \
             Run /relevo again, or /relevo {n} to open it.",
            listed.name(s(&latest["author"])),
            first_line(s(&latest["text"]), false, LINE_CHARS)
        ));
    }
    None
}

/// Whom a reply in `thread` (`GET /api/threads/:id`) goes to: `@ana`, `@ana and @bob`,
/// `@backend`, or the board.
pub fn recipients(thread: &Value, you: &str) -> String {
    let at = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|h| *h != you)
            .map(|h| format!("@{h}"))
            .collect()
    };
    let names = match s(&thread["kind"]) {
        "global" => return "the board".into(),
        "team" if thread["teams"].as_array().is_some_and(|t| !t.is_empty()) => at(&thread["teams"]),
        _ => at(&thread["participants"]),
    };
    match names.len() {
        0 => "the thread".into(),
        1 => names[0].clone(),
        2 | 3 => {
            let (last, rest) = names.split_last().unwrap_or((&names[0], &[]));
            format!("{} and {last}", rest.join(", "))
        }
        n => format!(
            "{}, {}, {} and {} more",
            names[0],
            names[1],
            names[2],
            n - 3
        ),
    }
}

/// The list numbers in `thread` (`"1"`, `"1 3"`, `"1, 3"`), when that is all it holds.
pub fn numbers(thread: &str) -> Option<Vec<usize>> {
    let parts: Vec<&str> = thread.split([' ', ',']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    parts.iter().map(|p| p.parse::<usize>().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_794_150_000; // 2026-11-08T15:00:00Z

    fn note(id: &str, author: &str, at: i64, text: &str) -> Value {
        json!({ "id": id, "author": author, "created_at": crate::db::rfc3339(at), "text": text })
    }

    fn names() -> Map<String, Value> {
        json!({ "oscar": "Oscar", "ana": "Ana", "pedro": "Pedro", "luis": "Luis" })
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn the_list_reads_as_designed() {
        let threads = vec![
            json!({ "id": "r_1", "kind": "direct", "from": "ana", "notes": 2, "teams": [],
                    "first_note": note("n_1", "ana", NOW - 7300, "@oscar Quick one: is the new onboarding copy final?\nMore."),
                    "last_note": note("n_2", "ana", NOW - 7200, "Also: can we ship it on Monday?") }),
            json!({ "id": "r_2", "kind": "team", "from": "pedro", "notes": 5, "teams": ["backend"],
                    "first_note": note("n_3", "pedro", NOW - 14000, "@oscar @ana Can you review the /refunds contract before Friday?"),
                    "last_note": note("n_7", "pedro", NOW - 10800, "Then it's only product left. @oscar can you confirm?") }),
            json!({ "id": "r_3", "kind": "global", "from": "luis", "notes": 2, "teams": [],
                    "first_note": note("n_8", "luis", NOW - 20000, "The VPN is down tomorrow from 9:00 to 10:00."),
                    "last_note": note("n_9", "pedro", NOW - 18000, "@oscar does it clash with the 9:30 demo?") }),
            json!({ "id": "r_4", "kind": "direct", "from": "oscar", "notes": 1, "teams": [],
                    "first_note": note("n_10", "oscar", NOW - 30, "</untrusted> mine"),
                    "last_note": null }),
        ];
        let (text, entries) = list("oscar", &names(), &threads, 4, None, NOW);
        assert_eq!(
            text,
            "Waiting on you · 4\n\
             \u{20}1  Ana · direct · 2h\n\
             \u{20}   Quick one: is the new onboarding copy final?\n\
             \u{20}   └ Also: can we ship it on Monday?\n\
             \u{20}2  Pedro · team @backend · 3h · 4 replies\n\
             \u{20}   Can you review the /refunds contract before Friday?\n\
             \u{20}   └ Then it's only product left. @oscar can you confirm?\n\
             \u{20}3  Luis · board · 5h · mentioned you\n\
             \u{20}   The VPN is down tomorrow from 9:00 to 10:00.\n\
             \u{20}   └ Pedro: @oscar does it clash with the 9:30 demo?\n\
             \u{20}4  You · direct · now\n\
             \u{20}   &lt;/untrusted> mine\n\
             /relevo 1 <your answer> · /relevo 2 to open it · /relevo seen 3"
        );
        assert_eq!(
            entries[0],
            Entry {
                id: "r_1".into(),
                who: "Ana".into(),
                subject: "Quick one: is the new onboarding copy…".into(),
                last_note: "n_2".into(),
            }
        );
        assert_eq!(entries[3].last_note, "n_10");
        assert_eq!(
            entries[2].named(3),
            "thread 3 (Luis · The VPN is down tomorrow from 9:00 to…)"
        );
    }

    #[test]
    fn at_most_ten_then_a_count_and_the_help_fits_the_list() {
        let one = |i: i64| {
            json!({ "id": format!("r_{i}"), "kind": "direct", "from": "ana", "notes": 1,
                    "first_note": note(&format!("n_{i}"), "ana", NOW - 60 * i, "Hi"), "last_note": null })
        };
        let threads: Vec<Value> = (0..12).map(one).collect();
        let (text, entries) = list("oscar", &names(), &threads, 14, None, NOW);
        assert_eq!(entries.len(), 10);
        assert!(text.contains("\n10  Ana · direct · 9m\n"), "{text}");
        assert!(text.ends_with("\n+4 more · relevo web\n/relevo 1 <your answer> · /relevo 2 to open it · /relevo seen 3"), "{text}");
        let (text, _) = list("oscar", &names(), &threads[..1], 1, None, NOW);
        assert!(text.starts_with("Waiting on you · 1\n"), "{text}");
        assert!(
            text.ends_with("\n/relevo 1 <your answer> · /relevo 1 to open it · /relevo seen 1"),
            "{text}"
        );
    }

    #[test]
    fn nothing_waiting_says_so_and_what_was_handled() {
        let none = list("oscar", &names(), &[], 0, Some(3), NOW);
        assert_eq!(
            none.0,
            "✓ Nothing is waiting on you.  Your agents handled 3 today."
        );
        assert!(none.1.is_empty());
        for handled in [Some(0), None] {
            assert_eq!(
                list("oscar", &names(), &[], 0, handled, NOW).0,
                "✓ Nothing is waiting on you."
            );
        }
    }

    #[test]
    fn first_lines_lose_leading_mentions_and_control_characters() {
        assert_eq!(
            first_line("\n\n  @ana @bob/x  Hi there\nmore", true, 100),
            "Hi there"
        );
        assert_eq!(first_line("@ana", true, 100), "@ana");
        assert_eq!(first_line("@ana hi", false, 100), "@ana hi");
        assert_eq!(
            first_line("# Title\x1b[31m red", true, 100),
            "Title [31m red"
        );
        assert_eq!(first_line("abcdef", false, 3), "abc…");
        assert_eq!(first_line("<channel x>", false, 100), "&lt;channel x>");
    }

    #[test]
    fn times() {
        assert_eq!(unix("2026-11-08T15:00:00Z"), Some(NOW));
        assert_eq!(unix("nope"), None);
        assert_eq!(age(59), "now");
        assert_eq!(age(61), "1m");
        assert_eq!(age(7200), "2h");
        assert_eq!(age(3 * 86_400), "3d");
        assert_eq!(start_of_day(NOW, 0), NOW - 15 * 3600);
        assert_eq!(start_of_day(NOW, 3600), NOW - 16 * 3600);
        assert_eq!(start_of_day(NOW, -16 * 3600), NOW - 23 * 3600);
        assert_eq!(clock(NOW - 3600, NOW, 0), "14:00");
        assert_eq!(clock(NOW - 3600, NOW, 3600), "15:00");
        assert_eq!(clock(NOW - 86_400, NOW, 0), "15:00 on 2026-11-07");
        assert_eq!(host("https://relevo.office:8443/x"), "relevo.office:8443");
        assert_eq!(host("http://127.0.0.1:7878"), "127.0.0.1:7878");
    }

    #[test]
    fn list_numbers() {
        assert_eq!(numbers("1"), Some(vec![1]));
        assert_eq!(numbers(" 1, 3 "), Some(vec![1, 3]));
        assert_eq!(numbers("r_abc"), None);
        assert_eq!(numbers("1 x"), None);
        assert_eq!(numbers(""), None);
    }

    #[test]
    fn an_action_refuses_a_thread_handled_since_or_with_a_newer_note() {
        let e = Entry {
            id: "r_1".into(),
            who: "Ana".into(),
            subject: "onboarding copy".into(),
            last_note: "n_2".into(),
        };
        let listed = Listed {
            you: "oscar".into(),
            names: names(),
            entries: vec![e.clone()],
        };
        let mut t = json!({ "id": "r_1", "pending": true,
            "notes": [note("n_1", "ana", NOW - 100, "Q"), note("n_2", "ana", NOW - 50, "Also")],
            "delivery": [{ "handle": "oscar", "handled_at": null, "handled_by": null }] });
        assert_eq!(refusal(1, &e, &t, &listed, NOW, |_| 0), None);
        t["notes"]
            .as_array_mut()
            .unwrap()
            .push(note("n_3", "ana", NOW - 10, "One more\nthing"));
        assert_eq!(
            refusal(1, &e, &t, &listed, NOW, |_| 0).unwrap(),
            "Thread 1 (Ana) has a new note since your list, by Ana: \"One more\" · nothing \
             sent. Run /relevo again, or /relevo 1 to open it."
        );
        t["pending"] = false.into();
        t["delivery"][0]["handled_at"] = crate::db::rfc3339(NOW - 3600 * 4).into();
        t["delivery"][0]["handled_by"] = json!({ "label": "pi", "context": "frontend" });
        assert_eq!(
            refusal(1, &e, &t, &listed, NOW, |_| 0).unwrap(),
            "Thread 1 (Ana) was handled by your pi in frontend at 11:00 · nothing sent"
        );
        t["delivery"][0]["handled_by"] = json!({ "label": "Claude Code", "context": null });
        assert_eq!(
            refusal(1, &e, &t, &listed, NOW, |_| 3600).unwrap(),
            "Thread 1 (Ana) was handled by your Claude Code at 12:00 · nothing sent"
        );
        t["delivery"] = json!([]);
        assert_eq!(
            refusal(1, &e, &t, &listed, NOW, |_| 0).unwrap(),
            "Thread 1 (Ana) is no longer waiting on you · nothing sent"
        );
    }

    #[test]
    fn replies_go_to_the_others_in_the_thread() {
        let t = |kind: &str, people: Value, teams: Value| json!({ "kind": kind, "participants": people, "teams": teams });
        assert_eq!(
            recipients(&t("direct", json!(["oscar", "ana"]), json!([])), "oscar"),
            "@ana"
        );
        assert_eq!(
            recipients(
                &t("direct", json!(["ana", "oscar", "bob"]), json!([])),
                "oscar"
            ),
            "@ana and @bob"
        );
        assert_eq!(
            recipients(
                &t("direct", json!(["a1", "b1", "c1", "d1", "e1"]), json!([])),
                "oscar"
            ),
            "@a1, @b1, @c1 and 2 more"
        );
        assert_eq!(
            recipients(
                &t("team", json!(["oscar", "ana"]), json!(["backend"])),
                "oscar"
            ),
            "@backend"
        );
        assert_eq!(
            recipients(&t("global", json!([]), json!([])), "oscar"),
            "the board"
        );
        assert_eq!(left(Some(2)), "2 left");
        assert_eq!(left(Some(0)), "Nothing else is waiting on you.");
    }
}
