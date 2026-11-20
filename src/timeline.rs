//! The timeline (`docs/protocol.md`, "timeline"): what a person reads in `relevo web`. Read only:
//! nothing here writes, so no note reaches a session, nothing is handled or claimed, and no
//! session is created or seen (the token alone says who asks: see `auth::Reader`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::Serialize;
use serde_json::{Value, json};

use crate::App;
use crate::auth::Reader;
use crate::db::{IS_BOT_SQL, SYSTEM_PERSON_SQL, rfc3339};
use crate::error::ApiError;

/// `first_note`, `last_note` and `last_other_note` are cut to this many characters.
const TEXT_CHARS: usize = 1000;
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    /// The `global` threads of the person.
    Board,
    /// Their `direct` and `team` threads, and the `global` threads that name them.
    Threads,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Show {
    All,
    Waiting,
    Answered,
    /// Accepted through 0.x, for pages of 0.4.x: threads no longer close, so it lists nothing.
    Closed,
}

/// `GET /api/timeline`'s query string, checked.
#[derive(Debug)]
struct Params {
    tab: Tab,
    show: Show,
    team: Option<String>,
    with: Option<String>,
    /// The `cursor` of the last thread of the previous page: its latest note's `seq`.
    before: Option<i64>,
    limit: usize,
    /// Unix seconds: count the threads the person has handled since then (`counts.handled`).
    handled_since: Option<i64>,
}

fn parse_params(
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Params, ApiError> {
    let Query(pairs) = query.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let mut seen: Vec<String> = Vec::new();
    let (mut tab, mut show, mut team, mut with, mut before, mut limit) =
        (None, None, None, None, None, None);
    let mut handled_since = None;
    for (name, value) in pairs {
        if seen.contains(&name) {
            return Err(ApiError::bad_request(format!("'{name}' given twice")));
        }
        seen.push(name.clone());
        match name.as_str() {
            "tab" => {
                tab = Some(match value.as_str() {
                    "board" => Tab::Board,
                    "threads" => Tab::Threads,
                    _ => return Err(ApiError::bad_request("'tab' must be board or threads")),
                })
            }
            "show" => {
                show = Some(match value.as_str() {
                    "all" => Show::All,
                    "waiting" => Show::Waiting,
                    "answered" => Show::Answered,
                    "closed" => Show::Closed,
                    _ => {
                        return Err(ApiError::bad_request(
                            "'show' must be all, waiting or answered",
                        ));
                    }
                })
            }
            "team" if !value.is_empty() => team = Some(value.to_lowercase()),
            "with" if !value.is_empty() => with = Some(value.to_lowercase()),
            "team" | "with" => return Err(ApiError::bad_request(format!("'{name}' is empty"))),
            "before" => {
                before = Some(
                    value
                        .parse::<i64>()
                        .ok()
                        .filter(|b| *b > 0)
                        .ok_or_else(|| ApiError::bad_request("'before' is not a cursor"))?,
                )
            }
            "limit" => {
                limit = Some(
                    value
                        .parse::<usize>()
                        .ok()
                        .filter(|l| (1..=MAX_LIMIT).contains(l))
                        .ok_or_else(|| ApiError::bad_request("'limit' must be 1 to 50"))?,
                )
            }
            "handled_since" => {
                handled_since = Some(value.parse::<i64>().ok().filter(|t| *t >= 0).ok_or_else(
                    || ApiError::bad_request("'handled_since' is not a time in unix seconds"),
                )?)
            }
            _ => return Err(ApiError::bad_request(format!("unknown parameter '{name}'"))),
        }
    }
    let tab = tab.ok_or_else(|| ApiError::bad_request("'tab' is required: board or threads"))?;
    if tab == Tab::Board && (show.is_some() || team.is_some() || with.is_some()) {
        return Err(ApiError::bad_request(
            "'show', 'team' and 'with' go with tab=threads only",
        ));
    }
    Ok(Params {
        tab,
        show: show.unwrap_or(Show::All),
        team,
        with,
        before,
        limit: limit.unwrap_or(DEFAULT_LIMIT),
        handled_since,
    })
}

// ---------- shapes ----------

#[derive(Serialize, Clone)]
pub struct NoteOut {
    id: String,
    author: String,
    via: String,
    /// The writing session's context then; `None` for a general one.
    context: Option<String>,
    created_at: String,
    text: String,
    truncated: bool,
    /// Written by the server's own person: the release notes (left out otherwise).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    system: bool,
}

/// `notes`: how many, in a page; every note, in one thread.
#[derive(Serialize)]
#[serde(untagged)]
pub enum Notes {
    Count(i64),
    All(Vec<NoteOut>),
}

#[derive(Serialize)]
pub struct ThreadOut {
    id: String,
    kind: String,
    title: String,
    from: String,
    /// Empty for a `global` thread: it would list everyone.
    participants: Vec<String>,
    teams: Vec<String>,
    /// Always `"open"`: threads no longer close (kept through 0.x for clients that read it).
    status: &'static str,
    named: bool,
    pending: bool,
    notes: Notes,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_note: Option<NoteOut>,
    /// In a page: `Some(None)` (null) when the thread holds one note.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_note: Option<Option<NoteOut>>,
    /// In a page: the latest note by someone else; `Some(None)` (null) when there is none, or it
    /// is the first note.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_other_note: Option<Option<NoteOut>>,
    handled_by_you: bool,
    cursor: String,
    /// In one thread: the name of everyone it names.
    #[serde(skip_serializing_if = "Option::is_none")]
    names: Option<BTreeMap<String, String>>,
    /// In one thread: who of them is marked as a bot.
    #[serde(skip_serializing_if = "Option::is_none")]
    bots: Option<Vec<String>>,
}

/// One thread of the person, as the queries read it.
struct Row {
    id: String,
    kind: String,
    title: String,
    from: String,
    named: bool,
    pending: bool,
    last_seq: i64,
    first_seq: i64,
    notes: i64,
    handled: bool,
    /// The latest note by someone else than the person; 0 when nobody else has written.
    last_other_seq: i64,
}

// ---------- queries ----------

/// A thread of person `?1` (`p` is their participant row). The first and latest notes come by
/// their keys, `notes` counts the entries of `notes_by_thread`. `handled_by_you` as in `thread`'s
/// delivery: the person has handled the latest note by someone else.
const ROW_SQL: &str = "
SELECT t.id, t.kind, t.title, o.handle, p.named_seq IS NOT NULL, p.waiting, p.last_seq,
       (SELECT min(n.seq) FROM notes n WHERE n.thread_id = t.id),
       (SELECT count(*) FROM notes n WHERE n.thread_id = t.id),
       p.last_other_seq > 0 AND coalesce(p.handled_seq, 0) >= p.last_other_seq,
       p.last_other_seq
FROM participants p INDEXED BY participants_timeline
JOIN threads t ON t.id = p.thread_id
JOIN people o ON o.id = t.opener_id";

/// What `tab=threads` lists: the person's (`p`) `direct` and `team` threads, and the `global`
/// threads that name them.
const FOR_YOU: &str = "(t.kind != 'global' OR p.named_seq IS NOT NULL)";

/// Opened by someone else, and the person (`p`) has written in it.
const ANSWERED: &str = "t.opener_id != p.person_id
    AND EXISTS (SELECT 1 FROM notes w WHERE w.thread_id = t.id AND w.author_id = p.person_id)";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        kind: r.get(1)?,
        title: r.get(2)?,
        from: r.get(3)?,
        named: r.get(4)?,
        pending: r.get(5)?,
        last_seq: r.get(6)?,
        first_seq: r.get(7)?,
        notes: r.get(8)?,
        handled: r.get(9)?,
        last_other_seq: r.get(10)?,
    })
}

/// The page's query: the person's threads in the tab, past the cursor, newest first, one more
/// than the page to know whether there is another. `?1` person, `?2` before, `?3` team, `?4` the
/// person of `with`.
fn page_sql(params: &Params) -> String {
    let tab = match params.tab {
        Tab::Board => "t.kind = 'global'",
        Tab::Threads => FOR_YOU,
    };
    let show = match params.show {
        Show::All => "1",
        Show::Waiting => "p.waiting = 1",
        Show::Answered => ANSWERED,
        Show::Closed => "0",
    };
    format!(
        "{ROW_SQL}
         WHERE p.person_id = ?1 AND p.last_seq < ?2 AND {tab} AND {show}
           AND (?3 IS NULL OR EXISTS (SELECT 1 FROM thread_teams tt
                                      WHERE tt.thread_id = t.id AND tt.name = ?3))
           AND (?4 IS NULL OR EXISTS (SELECT 1 FROM participants w
                                      WHERE w.thread_id = t.id AND w.person_id = ?4))
         ORDER BY p.last_seq DESC LIMIT {}",
        params.limit + 1
    )
}

/// What each tab, `show` value and team of the person's would list, in one pass over the
/// person's participant rows.
fn counts_sql() -> String {
    format!(
        "SELECT coalesce(sum(t.kind = 'global'), 0),
                coalesce(sum({FOR_YOU}), 0),
                coalesce(sum({FOR_YOU} AND p.waiting), 0),
                coalesce(sum({FOR_YOU} AND {ANSWERED}), 0)
         FROM participants p JOIN threads t ON t.id = p.thread_id
         WHERE p.person_id = ?1"
    )
}

/// The threads the person (`?1`) has handled (answered, or marked seen, a note by someone else)
/// since `?2`: by `handled_at`, which says when they last did.
const HANDLED_SQL: &str = "
SELECT count(*) FROM participants p WHERE p.person_id = ?1 AND p.handled_at >= ?2";

const TEAM_COUNTS_SQL: &str = "
SELECT tt.name, count(*) FROM participants p
JOIN thread_teams tt ON tt.thread_id = p.thread_id
WHERE p.person_id = ?1
  AND tt.name IN (SELECT t.name FROM team_members m JOIN teams t ON t.id = m.team_id
                  WHERE m.person_id = ?1)
GROUP BY tt.name";

/// The people the person shares a `direct` or `team` thread with.
const PEOPLE_SQL: &str = "
SELECT DISTINCT pe.handle FROM participants me
JOIN threads t ON t.id = me.thread_id AND t.kind != 'global'
JOIN participants o ON o.thread_id = me.thread_id AND o.person_id != me.person_id
JOIN people pe ON pe.id = o.person_id
WHERE me.person_id = ?1 ORDER BY pe.handle";

/// The note columns, the text cut one character past `TEXT_CHARS` when `cut` (so a longer one is
/// known), with the context it was written in.
fn note_sql(cut: bool, filter: &str) -> String {
    let text = if cut {
        format!("substr(n.text, 1, {})", TEXT_CHARS + 1)
    } else {
        "n.text".to_string()
    };
    format!(
        "SELECT n.seq, n.id, a.handle, n.via, c.context, n.created_at, {text},
                n.author_id IS {SYSTEM_PERSON_SQL}
         FROM notes n JOIN people a ON a.id = n.author_id
         LEFT JOIN note_contexts c ON c.seq = n.seq
         {filter}"
    )
}

fn note(r: &rusqlite::Row, cut: bool) -> rusqlite::Result<(i64, NoteOut)> {
    let mut text: String = r.get(6)?;
    let mut truncated = false;
    if cut && let Some((at, _)) = text.char_indices().nth(TEXT_CHARS) {
        text.truncate(at);
        truncated = true;
    }
    Ok((
        r.get(0)?,
        NoteOut {
            id: r.get(1)?,
            author: r.get(2)?,
            via: r.get(3)?,
            context: r.get(4)?,
            created_at: rfc3339(r.get(5)?),
            text,
            truncated,
            system: r.get(7)?,
        },
    ))
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// Runs `sql` (with one `IN (…)` list of `values`) and collects its rows.
fn rows_in<T>(
    conn: &Connection,
    sql: &str,
    values: &[SqlValue],
    f: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>, ApiError> {
    if values.is_empty() {
        return Ok(Vec::new());
    }
    let sql = sql.replace("(…)", &format!("({})", placeholders(values.len())));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), f)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn texts<'a>(items: impl IntoIterator<Item = &'a String>) -> Vec<SqlValue> {
    items
        .into_iter()
        .map(|s| SqlValue::Text(s.clone()))
        .collect()
}

/// The threads of `rows`, in one query for each of their participants, teams and notes (not one
/// per thread).
fn threads_out(conn: &Connection, rows: Vec<Row>) -> Result<Vec<ThreadOut>, ApiError> {
    let ids: Vec<SqlValue> = texts(rows.iter().map(|r| &r.id));
    let not_global: Vec<SqlValue> =
        texts(rows.iter().filter(|r| r.kind != "global").map(|r| &r.id));
    let mut participants: HashMap<String, Vec<String>> = HashMap::new();
    for (thread, handle) in rows_in(
        conn,
        "SELECT p.thread_id, pe.handle FROM participants p JOIN people pe ON pe.id = p.person_id
         WHERE p.thread_id IN (…) ORDER BY p.rowid",
        &not_global,
        |r| Ok((r.get::<_, String>(0)?, r.get(1)?)),
    )? {
        participants.entry(thread).or_default().push(handle);
    }
    let mut teams: HashMap<String, Vec<String>> = HashMap::new();
    for (thread, name) in rows_in(
        conn,
        "SELECT thread_id, name FROM thread_teams WHERE thread_id IN (…) ORDER BY name",
        &ids,
        |r| Ok((r.get::<_, String>(0)?, r.get(1)?)),
    )? {
        teams.entry(thread).or_default().push(name);
    }
    let seqs: Vec<SqlValue> = rows
        .iter()
        .flat_map(|r| [r.first_seq, r.last_seq, r.last_other_seq])
        .filter(|&seq| seq > 0)
        .collect::<BTreeSet<i64>>()
        .into_iter()
        .map(SqlValue::Integer)
        .collect();
    let notes: HashMap<i64, NoteOut> =
        rows_in(conn, &note_sql(true, "WHERE n.seq IN (…)"), &seqs, |r| {
            note(r, true)
        })?
        .into_iter()
        .collect();
    Ok(rows
        .into_iter()
        .map(|r| {
            let note = |seq: i64| (seq > 0 && seq != r.first_seq).then(|| notes.get(&seq).cloned());
            let last = note(r.last_seq).flatten();
            let other = note(r.last_other_seq).flatten();
            let first = notes.get(&r.first_seq).cloned();
            ThreadOut {
                participants: participants.remove(&r.id).unwrap_or_default(),
                teams: teams.remove(&r.id).unwrap_or_default(),
                notes: Notes::Count(r.notes),
                first_note: first,
                last_note: Some(last),
                last_other_note: Some(other),
                ..thread_out(r)
            }
        })
        .collect())
}

/// The fields every shape of a thread has.
fn thread_out(r: Row) -> ThreadOut {
    ThreadOut {
        id: r.id,
        kind: r.kind,
        title: r.title,
        from: r.from,
        participants: Vec::new(),
        teams: Vec::new(),
        status: "open",
        named: r.named,
        pending: r.pending,
        notes: Notes::Count(r.notes),
        first_note: None,
        last_note: None,
        last_other_note: None,
        handled_by_you: r.handled,
        cursor: r.last_seq.to_string(),
        names: None,
        bots: None,
    }
}

/// The name of each of these people, and those of them marked as bots, sorted.
fn names(
    conn: &Connection,
    handles: BTreeSet<String>,
) -> Result<(BTreeMap<String, String>, Vec<String>), ApiError> {
    let handles: Vec<SqlValue> = texts(&handles);
    let rows: Vec<(String, String, bool)> = rows_in(
        conn,
        &format!("SELECT handle, name, {IS_BOT_SQL} FROM people WHERE handle IN (…)"),
        &handles,
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut bots: Vec<String> = rows
        .iter()
        .filter(|(_, _, bot)| *bot)
        .map(|(handle, _, _)| handle.clone())
        .collect();
    bots.sort();
    let names = rows.into_iter().map(|(h, n, _)| (h, n)).collect();
    Ok((names, bots))
}

/// Every handle a thread names: its opener, its participants and the authors of its notes.
fn handles_of(t: &ThreadOut, into: &mut BTreeSet<String>) {
    into.insert(t.from.clone());
    into.extend(t.participants.iter().cloned());
    let last = t.last_note.as_ref().and_then(Option::as_ref);
    let other = t.last_other_note.as_ref().and_then(Option::as_ref);
    for note in t.first_note.iter().chain(last).chain(other) {
        into.insert(note.author.clone());
    }
    if let Notes::All(notes) = &t.notes {
        into.extend(notes.iter().map(|n| n.author.clone()));
    }
}

// ---------- handlers ----------

/// `GET /api/timeline`.
pub async fn list(
    State(app): State<Arc<App>>,
    reader: Reader,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let params = parse_params(query)?;
    let conn = app.db();
    let me = reader.person_id;
    let team = match &params.team {
        Some(name) => {
            let known: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM teams WHERE name = ?1)",
                [name],
                |r| r.get(0),
            )?;
            if !known {
                return Err(ApiError::bad_request(format!("no team named '{name}'")));
            }
            Some(name.clone())
        }
        None => None,
    };
    let with: Option<i64> = match &params.with {
        Some(handle) => Some(
            conn.query_row("SELECT id FROM people WHERE handle = ?1", [handle], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| ApiError::bad_request(format!("no person with handle '{handle}'")))?,
        ),
        None => None,
    };

    let mut rows: Vec<Row> = conn
        .prepare(&page_sql(&params))?
        .query_map(
            params![me, params.before.unwrap_or(i64::MAX), team, with],
            row,
        )?
        .collect::<rusqlite::Result<_>>()?;
    let more = rows.len() > params.limit;
    rows.truncate(params.limit);
    let cursor = more.then(|| rows.last().map(|r| r.last_seq.to_string()));
    let threads = threads_out(&conn, rows)?;

    let (board, threads_count, waiting, answered): (i64, i64, i64, i64) =
        conn.query_row(&counts_sql(), [me], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    let teams: Vec<String> = conn
        .prepare(
            "SELECT t.name FROM team_members m JOIN teams t ON t.id = m.team_id
             WHERE m.person_id = ?1 ORDER BY t.name",
        )?
        .query_map([me], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut team_counts: BTreeMap<String, i64> = teams.iter().map(|t| (t.clone(), 0)).collect();
    let mut stmt = conn.prepare(TEAM_COUNTS_SQL)?;
    for counted in stmt.query_map([me], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))? {
        let (name, n) = counted?;
        team_counts.insert(name, n);
    }
    let mut counts = json!({
        "board": board,
        "threads": threads_count,
        "waiting": waiting,
        "answered": answered,
        "teams": team_counts,
    });
    if let Some(since) = params.handled_since {
        let handled: i64 = conn.query_row(HANDLED_SQL, params![me, since], |r| r.get(0))?;
        counts["handled"] = handled.into();
    }
    let people: Vec<String> = conn
        .prepare(PEOPLE_SQL)?
        .query_map([me], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    let mut named: BTreeSet<String> = people.iter().cloned().collect();
    named.insert(reader.handle.clone());
    for t in &threads {
        handles_of(t, &mut named);
    }
    let (names, bots) = names(&conn, named)?;
    tracing::info!(
        person = %reader.handle,
        tab = ?params.tab,
        threads = threads.len(),
        "timeline"
    );
    Ok(Json(json!({
        "you": reader.handle,
        "teams": teams,
        "counts": counts,
        "people": people,
        "names": names,
        "bots": bots,
        "threads": threads,
        "cursor": cursor.flatten(),
    })))
}

/// `GET /api/timeline/threads/:id`: one thread of the person, every note in full.
pub async fn thread(
    State(app): State<Arc<App>>,
    reader: Reader,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<ThreadOut>, ApiError> {
    let Path(id) = id.map_err(|_| ApiError::not_found("no such thread"))?;
    let conn = app.db();
    let row = conn
        .query_row(
            &format!("{ROW_SQL} WHERE p.person_id = ?1 AND p.thread_id = ?2"),
            params![reader.person_id, id],
            row,
        )
        .optional()?
        .ok_or_else(|| ApiError::not_found("no such thread"))?;
    let mut out = threads_out(&conn, vec![row])?
        .pop()
        .ok_or_else(ApiError::internal)?;
    let notes: Vec<NoteOut> = conn
        .prepare(&note_sql(false, "WHERE n.thread_id = ?1 ORDER BY n.seq"))?
        .query_map([&out.id], |r| note(r, false).map(|(_, n)| n))?
        .collect::<rusqlite::Result<_>>()?;
    out.notes = Notes::All(notes);
    out.first_note = None;
    out.last_note = None;
    out.last_other_note = None;
    let mut named = BTreeSet::from([reader.handle.clone()]);
    handles_of(&out, &mut named);
    let (names, bots) = names(&conn, named)?;
    out.names = Some(names);
    out.bots = Some(bots);
    tracing::info!(person = %reader.handle, thread = %out.id, "timeline thread");
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(sql: &str, n: usize) -> Vec<String> {
        let conn = crate::db::open(":memory:").unwrap();
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let values: Vec<SqlValue> = (0..n).map(|i| SqlValue::Integer(i as i64 + 1)).collect();
        stmt.query_map(params_from_iter(&values), |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// `handled_since` counts from its own index, not the person's every row.
    #[test]
    fn the_handled_count_reads_its_index() {
        let plan = plan(HANDLED_SQL, 2);
        assert!(
            plan[0].contains(
                "USING COVERING INDEX participants_handled (person_id=? AND handled_at>?)"
            ),
            "{plan:#?}"
        );
    }

    /// A page reads the person's rows from `participants_timeline`, already newest first, and
    /// stops after the page: no sort, and no table read whole.
    #[test]
    fn a_page_reads_the_persons_threads_in_order() {
        for tab in [Tab::Board, Tab::Threads] {
            // (`Show::Closed` lists nothing, whatever the plan.)
            for show in [Show::All, Show::Waiting, Show::Answered] {
                let params = Params {
                    tab,
                    show,
                    team: None,
                    with: None,
                    before: None,
                    limit: 20,
                    handled_since: None,
                };
                let plan = plan(&page_sql(&params), 4);
                assert!(
                    plan[0].starts_with(
                        "SEARCH p USING INDEX participants_timeline (person_id=? AND last_seq<?)"
                    ),
                    "{plan:#?}"
                );
                assert!(!plan.iter().any(|s| s.contains("TEMP B-TREE")), "{plan:#?}");
                assert!(!plan.iter().any(|s| s.starts_with("SCAN")), "{plan:#?}");
            }
        }
    }

    /// The counts read the person's rows by index, and each thread by its key.
    #[test]
    fn counts_read_only_the_persons_threads() {
        for (sql, n) in [
            (counts_sql().as_str(), 1),
            (TEAM_COUNTS_SQL, 1),
            (PEOPLE_SQL, 1),
            (HANDLED_SQL, 2),
        ] {
            let plan = plan(sql, n);
            assert!(plan.iter().any(|s| s.contains("person_id=?")), "{plan:#?}");
            assert!(
                !plan.iter().any(|s| s.starts_with("SCAN p")
                    || s.starts_with("SCAN t ")
                    || s.starts_with("SCAN n")),
                "{plan:#?}"
            );
        }
    }

    #[test]
    fn answered_reads_one_index_entry() {
        let plan = plan(
            "SELECT 1 FROM notes w WHERE w.thread_id = ?1 AND w.author_id = ?2",
            2,
        );
        assert!(
            plan[0].contains("notes_by_thread_author (thread_id=? AND author_id=?)"),
            "{plan:#?}"
        );
    }
}
