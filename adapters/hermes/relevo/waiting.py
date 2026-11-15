"""/relevo in Hermes (docs/agents.md, "The /relevo command" and the Hermes section): the threads
waiting on the person, numbered, from the read-only timeline, drawn for where the command was typed,
and the numbers kept per Hermes session for relevo_send and relevo_thread. The same list and the same
checks as relevo mcp's `waiting` (src/waiting.rs) and pi's (adapters/pi/src/waiting.ts).
"""

from __future__ import annotations

import re
import threading
import time
from collections import OrderedDict
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any, Callable, Optional

from . import client

# The most threads the list shows; the rest are counted.
MAX_SHOWN = 10
# How much of a note's first line the list shows.
LINE_CHARS = 100
# How much of a thread's first line names it in an action's answer.
SUBJECT_CHARS = 40
# The most pages of 50 the list reads (the oldest come last).
MAX_PAGES = 200
# The most Hermes sessions whose last list is kept.
MAX_SESSIONS = 100

USAGE = "/relevo · /relevo N <your answer> · /relevo N · /relevo seen N · /relevo @name <note>"


# -- the rows ---------------------------------------------------------------------------------------

@dataclass
class Row:
    n: int
    id: str
    who: str  # whoever opened it ("You" for the person)
    kind: str  # direct, team @backend, board
    age: str  # since its latest note: now, 5m, 2h, 3d
    replies: int  # said from two on (0 otherwise)
    mentioned: bool  # a board thread: it waits on the person because it names them
    first: str  # the first line of its first note, without the @ names it starts with
    reply_by: Optional[str]  # the latest note's author, when not the opener
    reply: Optional[str]  # the latest note's first line, when there is one
    subject: str  # its first line, short: names it in an action's answer
    last_note: str  # the latest note the list showed: an action refuses when a newer one came since


@dataclass
class Listed:
    """The last list shown in a session: whom it was for, the names it knew, and its rows."""
    you: str
    names: dict
    rows: list = field(default_factory=list)


def _name(names: dict, you: str, handle: str) -> str:
    if handle == you:
        return "You"
    name = names.get(handle)
    return name if isinstance(name, str) and name.strip() else handle


def cut(text: str, max_chars: int) -> str:
    """`text` cut to `max_chars` characters, at a word when one ends in the second half, with `…`."""
    if len(text) <= max_chars:
        return text
    head = text[:max_chars]
    at = max_chars
    for i in range(len(head) - 1, -1, -1):
        if head[i].isspace():
            if i >= max_chars // 2:
                at = i
            break
    return text[:at].rstrip() + "…"


_CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f]")


def first_line(text: str, mentions: bool, max_chars: int) -> str:
    """The first line of `text` that is not blank, without control characters and, with
    `mentions`, without the @ names it starts with: the person knows a note names them."""
    raw = next((line for line in re.split(r"\r?\n", text or "") if line.strip()), "")
    line = _CONTROL.sub(" ", raw).strip().lstrip("#").strip()
    if mentions:
        rest = line
        while re.match(r"@\w", rest) and not rest[1] == "_":
            parts = rest.split(None, 1)
            rest = parts[1].lstrip() if len(parts) > 1 else ""
        if rest:
            line = rest
    return client._defang(cut(line, max_chars))


def unix(at: Any) -> Optional[int]:
    """`2026-11-08T10:47:00Z` in unix seconds; None when it is not a time."""
    try:
        return int(datetime.fromisoformat(str(at).replace("Z", "+00:00")).timestamp())
    except (TypeError, ValueError):
        return None


def age(secs: int) -> str:
    s = max(0, secs)
    if s < 60:
        return "now"
    if s < 3600:
        return f"{s // 60}m"
    if s < 86_400:
        return f"{s // 3600}h"
    return f"{s // 86_400}d"


def start_of_day(now: int) -> int:
    """When the local day of `now` began."""
    return int(datetime.fromtimestamp(now).replace(hour=0, minute=0, second=0, microsecond=0).timestamp())


def clock(at: int, now: int) -> str:
    """`11:20` in local time when it is the day of `now`, else `11:20 on 2026-11-14`."""
    d = datetime.fromtimestamp(at)
    hm = d.strftime("%H:%M")
    return hm if start_of_day(at) == start_of_day(now) else f"{hm} on {d.strftime('%Y-%m-%d')}"


def host(url: str) -> str:
    rest = url.split("://", 1)[1] if "://" in url else url
    return re.split(r"[/?#]", rest)[0]


def _kind(t: dict) -> str:
    if t.get("kind") == "team":
        teams = [f"@{x}" for x in t.get("teams") or [] if isinstance(x, str)]
        return f"team {' '.join(teams)}" if teams else "team"
    if t.get("kind") == "global":
        return "board"
    return str(t.get("kind") or "")


def rows(you: str, names: dict, threads: list, now: int) -> list:
    """The rows of the list: `threads` are the timeline's, oldest first; at most MAX_SHOWN."""
    out = []
    for i, t in enumerate(threads[:MAX_SHOWN]):
        first = t.get("first_note") if isinstance(t.get("first_note"), dict) else None
        last = t.get("last_note") if isinstance(t.get("last_note"), dict) else None
        opener = (first or {}).get("author") or t.get("from") or ""
        latest = last or first or {}
        at = unix(latest.get("created_at"))
        text = (first or {}).get("text") if isinstance((first or {}).get("text"), str) else (t.get("title") or "")
        replies = (t.get("notes") if isinstance(t.get("notes"), int) else 1) - 1
        out.append(Row(
            n=i + 1, id=str(t.get("id") or ""), who=_name(names, you, opener), kind=_kind(t),
            age=age(now - (at if at is not None else now)), replies=replies if replies > 1 else 0,
            mentioned=t.get("kind") == "global", first=first_line(text, True, LINE_CHARS),
            reply_by=None if not last or last.get("author") == opener else _name(names, you, last.get("author") or ""),
            reply=first_line(last.get("text") or "", False, LINE_CHARS) if last else None,
            subject=first_line(text, True, SUBJECT_CHARS), last_note=str(latest.get("id") or "")))
    return out


def meta(r: Row) -> list:
    """`direct`, `2h`, `4 replies`, `mentioned you`: the parts after a row's name."""
    return [p for p in (r.kind, r.age, f"{r.replies} replies" if r.replies else "",
                        "mentioned you" if r.mentioned else "") if p]


def help_parts(count: int) -> list:
    return ["/relevo 1 <your answer>", f"/relevo {min(2, count)}", f"/relevo seen {min(3, count)}"]


# -- drawing it for where /relevo was typed -------------------------------------------------------------

class Plain:
    """Text as it is: a pager (Hermes' TUI, the desktop app) or a surface that says nothing of itself."""
    indent = "    "

    def esc(self, text: str) -> str:
        return text

    def bold(self, text: str) -> str:
        return text

    def italic(self, text: str) -> str:
        return text

    def number(self, text: str) -> str:
        return text

    def mention(self, text: str) -> str:
        return text

    def code(self, text: str) -> str:
        return text

    def ok(self, text: str) -> str:
        return text

    def bad(self, text: str) -> str:
        return text

    def num_col(self, n: int) -> str:
        return f"{n:>2}"

    def reply(self, text: str) -> str:
        return text


class Ansi(Plain):
    """The classic CLI, which prints a command's answer with its ANSI colours: as pi draws it."""
    _B, _D, _I, _R = "\x1b[1m", "\x1b[2m", "\x1b[3m", "\x1b[0m"

    def esc(self, text: str) -> str:
        # A teammate's text may not carry escape sequences of its own (control characters are gone already).
        return text.replace("\x1b", "")

    def bold(self, text: str) -> str:
        return f"{self._B}{text}{self._R}"

    def italic(self, text: str) -> str:
        return f"{self._D}{text}{self._R}"

    def number(self, text: str) -> str:
        return f"\x1b[1;33m{text}{self._R}"

    def mention(self, text: str) -> str:
        return f"\x1b[36m{text}\x1b[39m"

    def code(self, text: str) -> str:
        return text

    def ok(self, text: str) -> str:
        return f"\x1b[32m{text}\x1b[39m"

    def bad(self, text: str) -> str:
        return f"\x1b[31m{text}\x1b[39m"

    def reply(self, text: str) -> str:
        return f"{self._D}{self._I}{text}{self._R}"


class Markdown(Plain):
    """Markdown a messaging platform renders (Discord, Slack…, through each adapter's format_message):
    a teammate's text escaped with backslashes, as relevo mcp's md() does."""
    indent = ""

    def esc(self, text: str) -> str:
        out = []
        for i, line in enumerate(text.split("\n")):
            lead = len(line) - len(line.lstrip())
            head, body = line[:lead], line[lead:]
            digits = len(body) - len(body.lstrip("0123456789"))
            ordered = digits > 0 and body[digits:digits + 1] in (".", ")")
            chars = []
            for j, c in enumerate(body):
                if (j == 0 and c in "#-+=") or c in "\\*_`[]<>&|~" or (ordered and j == digits):
                    chars.append("\\")
                chars.append(c)
            out.append(head + "".join(chars))
        return "\n".join(out)

    def bold(self, text: str) -> str:
        return f"**{text}**"

    def italic(self, text: str) -> str:
        return f"*{text}*"

    def number(self, text: str) -> str:
        return f"**{text}**"

    def code(self, text: str) -> str:
        return f"`{text}`"

    def num_col(self, n: int) -> str:
        return str(n)

    def reply(self, text: str) -> str:
        return f"*{text}*"


class Telegram(Markdown):
    """Telegram: the gateway turns the markdown into MarkdownV2 itself (telegram_markdown.to_markdown_v2),
    which reads `*` runs as emphasis even after a backslash and shows the backslash. So a teammate's
    text is not escaped but disarmed: `*` and the backtick become look-alikes, and an invisible
    zero-width space breaks `~~`, `||`, `](`, `$$` and what would open a block at a line's start."""
    _ZW = "​"

    def esc(self, text: str) -> str:
        text = text.replace("*", "∗").replace("`", "ˋ")
        for pair in ("~~", "||", "](", "$$"):
            text = text.replace(pair, pair[0] + self._ZW + pair[1])
        return "\n".join(self._ZW + line if line[:1] in "#>-+*|<=!$" or line[:1].isdigit() else line
                         for line in text.split("\n"))



_MENTION = re.compile(r"@[^\W_][\w./@-]*")


def _mentions(style: Plain, text: str) -> str:
    """A teammate's text, escaped, with its @names drawn as mentions."""
    if type(style) is not Ansi:
        return style.esc(text)
    out, last = [], 0
    for m in _MENTION.finditer(text):
        at = m.start()
        if at > 0 and (text[at - 1].isalnum()):
            continue  # an email, not a mention
        name = m.group(0).rstrip(".-")
        out.append(style.esc(text[last:at]) + style.mention(style.esc(name)))
        last = at + len(name)
    return "".join(out) + style.esc(text[last:])


NOT_CONNECTED = ("Relevo isn't connected on this machine.", "Run ", "relevo connect <server>",
                 " with the token your admin gave you, or set RELEVO_URL and RELEVO_TOKEN in this Hermes profile.")


def render(shown: dict, style: Plain) -> str:
    """What /relevo shows (`shown` from waiting_list), drawn in `style`."""
    state = shown["state"]
    dot = style.italic(" · ") if type(style) is Ansi else " · "
    if state == "empty":
        out = style.ok("✓") + " Nothing is waiting on you."
        if shown.get("handled"):
            out += " " + style.italic(f"Your agents handled {shown['handled']} today.")
        return out
    if state == "offline":
        return (style.bad(style.bold(f"Relevo can't reach {style.esc(shown['host'])} since {shown['since']}."))
                + " " + style.italic("Notes you send now will fail; nothing is lost on the server."))
    if state == "not-connected":
        a, b, c, d = NOT_CONNECTED
        return style.bold(a) + " " + b + style.code(c) + d
    if state == "error":
        return style.bad(style.esc(shown["message"]))
    total, listed = shown["total"], shown["rows"]
    out = [style.bold("Waiting on you") + " · " + style.number(str(total)) if type(style) is Ansi
           else style.bold(f"Waiting on you · {total}")]
    for r in listed:
        head = (dot.join(style.italic(p) for p in meta(r)) if type(style) is Ansi
                else style.italic(style.esc(" · ".join(meta(r)))) if isinstance(style, Markdown)
                else " · ".join(meta(r)))
        out.append("")
        out.append(f"{style.number(style.num_col(r.n))}  {style.bold(style.esc(r.who))}{dot}{head}")
        out.append(style.indent + _mentions(style, r.first))
        if r.reply is not None:
            by = f"{r.reply_by}: " if r.reply_by else ""
            text = f"└ {by}{r.reply}"
            if type(style) is Ansi:
                out.append(style.indent + style.italic("└ ") + style.reply(_mentions(style, by + r.reply)))
            else:
                out.append(style.indent + style.reply(style.esc(text)))
    if total > MAX_SHOWN:
        out += ["", style.italic(f"+{total - MAX_SHOWN} more · relevo web")]
    a, b, c = help_parts(len(listed))
    if isinstance(style, Telegram):
        # Telegram nests no code in italics: the commands as code alone, which a tap copies.
        line = f"{style.code(a)} · {style.code(b)} to open it · {style.code(c)}"
    elif isinstance(style, Markdown):
        line = style.italic(f"{style.code(a)} · {style.code(b)} to open it · {style.code(c)}")
    else:
        line = style.italic(f"{a} · {b} to open it · {c}")
    out += ["", line]
    return "\n".join(out)


def style_for(platform: Any) -> Plain:
    """How /relevo draws its answer where it was typed: colours in the classic CLI (it prints a command's
    answer with its ANSI codes), Telegram's own markdown, markdown on the other messaging platforms,
    and plain text in a pager (TUI, desktop) or where the platform is not said."""
    p = str(platform or "").strip().lower()
    if p in ("cli", "local"):
        return Ansi()
    if p == "telegram":
        return Telegram()
    if p in ("", "tui", "desktop", "acp", "api_server", "codex", "webhook", "cron", "kanban", "tool"):
        return Plain()
    return Markdown()


# -- the numbers, per Hermes session ------------------------------------------------------------------

_lock = threading.Lock()
_lists: "OrderedDict[str, Listed]" = OrderedDict()
# When the first request of a run of failures failed (None: the server answers).
_offline_since: Optional[int] = None


def keep(keys: list, listed: Listed) -> None:
    with _lock:
        for key in keys:
            _lists[key] = listed
            _lists.move_to_end(key)
        while len(_lists) > MAX_SESSIONS:
            _lists.popitem(last=False)


def listed_for(keys: list) -> Optional[Listed]:
    with _lock:
        return next((_lists[k] for k in keys if k in _lists), None)


def command_keys(session_key: Any = "", session_id: Any = "") -> list:
    """The keys a command's list is kept under: the session's key and its id (a tool call may know
    either), or "" (one list for the process) when the surface says neither."""
    return [str(k) for k in (session_key, session_id) if k] or [""]


def tool_keys(session_id: Any = None) -> list:
    """The keys a tool call looks its session's list up by: the session key the gateway binds for its
    turns, the session id Hermes passes the tool, and the process's list when neither is known."""
    keys = []
    try:
        from gateway.session_context import get_session_env

        for name in ("HERMES_SESSION_KEY", "HERMES_SESSION_ID"):
            value = str(get_session_env(name, "") or "")
            if value:
                keys.append(value)
    except Exception:
        pass
    if session_id:
        keys.append(str(session_id))
    return keys or [""]


def resolve(listed: Optional[Listed], ns: list):
    """The rows of the last list that `ns` name, or why not (nothing sent)."""
    if listed is None:
        return "There is no list in this session yet: run /relevo first, then use its numbers. Nothing was sent."
    out = []
    for n in ns:
        if n < 1 or n > len(listed.rows):
            return (f"There is no thread {n} in your last list (1 to {len(listed.rows)}): run /relevo to see it "
                    "again. Nothing was sent." if listed.rows
                    else "Your last list had nothing waiting on you: run /relevo again. Nothing was sent.")
        row = listed.rows[n - 1]
        if row not in out:
            out.append(row)
    return out


def numbers(thread: Any) -> Optional[list]:
    """The list numbers in `thread` (`"1"`, `"1 3"`, `"1, 3"`), when that is all it holds."""
    if not isinstance(thread, str):
        return None
    parts = [p for p in re.split(r"[ ,]", thread) if p]
    if not parts or not all(p.isdigit() for p in parts):
        return None
    return [int(p) for p in parts]


# -- talking to the server ----------------------------------------------------------------------------

def unreachable(exc: BaseException) -> bool:
    """No answer from Relevo itself: none at all, or a proxy's 502, 503 or 504 without Relevo's error."""
    import httpx

    if isinstance(exc, client.RelevoError):
        return exc.code in ("HTTP_502", "HTTP_503", "HTTP_504")
    return isinstance(exc, httpx.TransportError)


def reached(exc: Optional[BaseException] = None) -> None:
    global _offline_since
    if exc is None:
        _offline_since = None
    elif unreachable(exc) and _offline_since is None:
        _offline_since = int(time.time())


def offline_line(url: str) -> str:
    now = int(time.time())
    return (f"Relevo can't reach {host(url)} since {clock(_offline_since or now, now)}. "
            "Notes you send now will fail; nothing is lost on the server.")


def failure(api: client.Api, exc: BaseException) -> str:
    if unreachable(exc):
        return offline_line(api.settings.url)
    if isinstance(exc, client.RelevoError):
        return client.describe_refusal(exc)
    if isinstance(exc, client.CertError):
        return f"{exc}."
    return f"could not reach the Relevo server ({client.describe_error(exc)})."


def _all_waiting(api: client.Api) -> tuple:
    threads, names, before = [], {}, None
    for _ in range(MAX_PAGES):
        params = {"tab": "threads", "show": "waiting", "limit": "50"}
        if before:
            params["before"] = before
        v = api.timeline(params)
        names.update(v.get("names") or {})
        got = v.get("threads") if isinstance(v.get("threads"), list) else []
        threads += got
        if not got or not isinstance(v.get("cursor"), str):
            counts = v.get("counts") or {}
            return str(v.get("you") or ""), names, threads[::-1], int(counts.get("waiting") or 0)
        before = v["cursor"]
    raise RuntimeError(f"more than {MAX_PAGES} pages of waiting threads")


def count_waiting(api: client.Api) -> Optional[int]:
    try:
        v = api.timeline({"tab": "threads", "show": "waiting", "limit": "1"})
        reached()
        n = (v.get("counts") or {}).get("waiting")
        return n if isinstance(n, int) else None
    except Exception as exc:
        reached(exc)
        return None


def waiting_list(keys: list, settings: Optional[client.Settings] = None) -> dict:
    """What /relevo shows: the list (kept under `keys` for its numbers), or why there is none."""
    try:
        settings = settings or client.resolve_settings()
    except client.CertError as exc:
        return {"state": "error", "message": f"{exc}."}
    if not settings.configured:
        return {"state": "not-connected"}
    if settings.cert_problem:
        return {"state": "error", "message": settings.cert_problem}
    api = client.Api(settings)
    now = int(time.time())
    try:
        you, names, threads, total = _all_waiting(api)
        reached()
    except Exception as exc:
        reached(exc)
        if unreachable(exc):
            return {"state": "offline", "host": host(settings.url), "since": clock(_offline_since or now, now)}
        if isinstance(exc, client.RelevoError) and exc.status == 404:
            return {"state": "error", "message": "This Relevo server has no timeline, which /relevo reads: it is "
                                                 "older than 0.4.0. Ask its owner to update it."}
        return {"state": "error", "message": failure(api, exc)}
    shown = rows(you, names, threads, now)
    keep(keys, Listed(you=you, names=names, rows=shown))
    total = max(total, len(threads))
    if shown:
        return {"state": "list", "rows": shown, "total": total}
    handled = None
    try:
        # A server before 0.6.0 refuses handled_since: the line is left out.
        v = api.timeline({"tab": "threads", "show": "waiting", "limit": "1",
                          "handled_since": str(start_of_day(now))})
        h = (v.get("counts") or {}).get("handled")
        handled = h if isinstance(h, int) else None
    except Exception:
        pass
    return {"state": "empty", "handled": handled}


# -- the answers of the tools to list numbers -----------------------------------------------------------

def _delivery(thread: dict, you: str) -> Optional[dict]:
    for d in thread.get("delivery") or []:
        if isinstance(d, dict) and d.get("handle") == you:
            return d
    return None


def refusal(row: Row, thread: dict, listed: Listed, now: int) -> Optional[str]:
    """Why an action on `row` must not go ahead, from the thread as it is now: it no longer waits on
    the person (another of their sessions handled it), or a note came after the one the list showed."""
    n, who = row.n, row.who
    if thread.get("pending") is not True:
        d = _delivery(thread, listed.you) or {}
        at = unix(d.get("handled_at")) if d.get("handled_at") else None
        out = f"Thread {n} ({who}) "
        if at is not None:
            out += "was handled"
            by = d.get("handled_by")
            if isinstance(by, dict):
                out += f" by your {by.get('label')}"
                if isinstance(by.get("context"), str):
                    out += f" in {by['context']}"
            out += f" at {clock(at, now)}"
        else:
            out += "is no longer waiting on you"
        return out + " · nothing sent"
    notes = thread.get("notes") or []
    latest = notes[-1] if notes else None
    if not latest or latest.get("id") == row.last_note:
        return None
    return (f"Thread {n} ({who}) has a new note since your list, by "
            f"{_name(listed.names, listed.you, latest.get('author') or '')}: "
            f"\"{first_line(latest.get('text') or '', False, LINE_CHARS)}\" · nothing sent. "
            f"Run /relevo again, or /relevo {n} to open it.")


def recipients(thread: dict, you: str) -> str:
    """Whom a reply in `thread` goes to: `@ana`, `@ana and @bob`, `@backend`, or the board."""
    def at(v: Any) -> list:
        return [f"@{h}" for h in v if isinstance(h, str) and h != you] if isinstance(v, list) else []

    if thread.get("kind") == "global":
        return "the board"
    names = at(thread.get("teams")) if thread.get("kind") == "team" and thread.get("teams") else at(
        thread.get("participants"))
    if not names:
        return "the thread"
    if len(names) == 1:
        return names[0]
    if len(names) <= 3:
        return f"{', '.join(names[:-1])} and {names[-1]}"
    return f"{names[0]}, {names[1]}, {names[2]} and {len(names) - 3} more"


def named(row: Row) -> str:
    return f"thread {row.n} ({row.who} · {row.subject})"


def left(waiting: Optional[int]) -> str:
    if waiting is None:
        return ""
    return "Nothing else is waiting on you." if waiting == 0 else f"{waiting} left"


def send_listed(api: client.Api, keys: list, ns: list, payload: dict, seen: bool,
                describe: Callable[[dict], str]) -> str:
    """relevo_send with list numbers in `thread`: each thread is read whole first (so what the person
    saw reaches this session, and `seen` and a reply handle it), and left alone when it no longer
    waits on the person or has a note newer than the list showed. As relevo mcp and pi do."""
    text = payload.get("text")
    if len(ns) > 1 and not (seen and text is None):
        return "Error: several list numbers go only with `seen: true` and no text (/relevo seen 1 3). Nothing was sent."
    listed = listed_for(keys)
    found = resolve(listed, ns)
    if isinstance(found, str):
        return "Error: " + found
    now = int(time.time())
    done, notes = [], []
    for row in found:
        try:
            thread = api.thread(row.id)
            reached()
        except Exception as exc:
            reached(exc)
            return "Error: " + failure(api, exc)
        no = refusal(row, thread, listed, now)
        if no:
            notes.append(no)
            continue
        try:
            result = api.send({**payload, "thread": row.id})
            reached()
        except Exception as exc:
            reached(exc)
            notes.append(f"Thread {row.n} ({row.who}): {failure(api, exc)}")
            continue
        if text is not None:
            line = f"Sent to {recipients(thread, listed.you)}: \"{text}\" · {named(row)}"
            added = [h for h in result.get("added") or [] if isinstance(h, str)]
            if added:
                line += f" · added {', '.join('@' + h for h in added)} to the thread"
            if result.get("claimed") == "other-session":
                line += " · another of your sessions holds this thread"
            done.append(line)
        else:
            done.append(named(row) if seen else describe(result))
    out = []
    if done:
        rest = left(count_waiting(api))
        if seen and text is None:
            out.append(f"Marked seen: {', '.join(done)}. Nothing was written. {rest}".rstrip())
        else:
            out.append("\n".join(x for x in ("\n".join(done), rest) if x))
    out += notes
    return "\n".join(out)


def thread_head(keys: list, ns: list):
    """For relevo_thread with a list number: (thread id, the line that names it) or why not."""
    if len(ns) != 1:
        return "Error: open one thread at a time: one list number."
    found = resolve(listed_for(keys), ns)
    if isinstance(found, str):
        return "Error: " + found
    row = found[0]
    return row.id, f"Thread {row.n} of your list ({row.who} · {row.subject}):\n"


# -- /relevo N, seen, @name: the request for the agent --------------------------------------------------

TERSE = ("Then reply with only the tool's result line(s), word for word, plus at most one short line if "
         "something I asked is left unanswered: no reasoning shown, no comment on what you did, no recap of "
         "other threads.")


def request(args: str, listed: Optional[Listed]) -> dict:
    """What the agent is asked to do for `/relevo <args>` (not the list): {"ask", "say"}, where `say`
    is how the person would put it to Hermes themselves; or {"refuse"}: something /relevo does not take,
    or a number the last list does not have (no turn starts). The request never starts with `/`, which
    the CLI would run as a command again."""
    def check(ns: list) -> Optional[dict]:
        found = resolve(listed, ns)
        return {"refuse": found} if isinstance(found, str) else None

    m = re.match(r"^seen\s+(.+)$", args, re.I | re.S)
    if m:
        ns = numbers(m.group(1).strip())
        if not ns:
            return {"refuse": USAGE}
        joined = " ".join(str(n) for n in ns)
        return check(ns) or {
            "ask": f"Relevo · I typed /relevo seen {joined}: mark these threads of my /relevo list seen. "
                   f"Call relevo_send {{ thread: \"{joined}\", seen: true }} once (the numbers as they are, never a "
                   f"thread id). {TERSE}",
            "say": f"mark {' and '.join(str(n) for n in ns)} seen"}
    m = re.match(r"^(\d+)(?:\s+(.+))?$", args, re.S)
    if m:
        n, text = int(m.group(1)), (m.group(2) or "").strip()
        if not text:
            return check([n]) or {
                "ask": f"Relevo · I typed /relevo {n}: open thread {n} of my /relevo list with relevo_thread "
                       f"{{ id: \"{n}\" }} (the number as it is, never a thread id), then tell me in one or two lines "
                       "what it needs from me and the next step you propose, and nothing else: no reasoning shown, "
                       "no recap of other threads. Send nothing.",
                "say": f"open {n}"}
        return check([n]) or {
            "ask": f"Relevo · I typed /relevo {n} {text}\n\nThat text is my answer to thread {n} of my /relevo list. "
                   "Write it as my note: in the first person, complete, adding nothing I did not say. "
                   f"Send it with relevo_send {{ thread: \"{n}\", text }} (the number as it is, never a thread id). "
                   f"{TERSE} If the text asks you to do something first (check, look up, find out) rather than "
                   "answer, do that, then show me the draft and ask before sending. When the tool says nothing "
                   "was sent, or fails, tell me what it said and stop.",
            "say": f"reply to {n} that {text}"}
    if args.startswith("@"):
        return {"ask": f"Relevo · I typed /relevo {args}\n\nA new note: send it with relevo_send {{ text }}, written "
                       "as the Relevo rules say. Then reply with only the tool's result line, word for word, and "
                       "under it the exact text sent: no reasoning shown, no recap of other threads.",
                "say": f"send {args}"}
    return {"refuse": USAGE}


def command(ctx: Any) -> Callable:
    """The /relevo handler for ctx.register_command."""

    def relevo(raw_args: str = "", *, session_key: str = "", session_id: str = "", platform: str = "",
               **_: Any) -> Optional[str]:
        # A note in a Relevo thread is a teammate's message: their `/relevo` is never run there, or
        # the person's list would be written in that thread for them to read.
        if str(platform or "").lower() == "relevo":
            return None
        style = style_for(platform)
        keys = command_keys(session_key, session_id)
        args = str(raw_args or "").strip()
        if not args:
            return render(waiting_list(keys), style)
        r = request(args, listed_for(keys))
        if "refuse" in r:
            return style.esc(r["refuse"])
        inject = getattr(ctx, "inject_message", None)
        try:
            handed = bool(inject and inject(r["ask"], session_key=str(session_key) or None))
        except Exception:
            handed = False
        if handed:
            return None
        # No turn can start from here (gateway injection is off for this plugin, or this Hermes has
        # none): the person says it to Hermes, and the tools take the numbers all the same.
        out = style.bold("Tell me:") + " " + style.esc(r["say"])
        if inject:
            plugin = getattr(ctx, "plugin_id", "relevo") or "relevo"
            out += "\n" + style.italic(style.esc(
                f"(/relevo can hand it to me itself once plugins.entries.{plugin}.allow_gateway_injection is "
                "true in this profile's config.)"))
        return out

    return relevo
