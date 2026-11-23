"""Relevo client tools (``relevo`` toolset): relevo_send, relevo_inbox, relevo_thread.

Same behaviour as the `relevo mcp` tools of docs/agents.md, calling the HTTP API directly (for the
session client.session_id picks). Sessions that have only these tools never see the relevo platform
hint, so the rules of docs/agents.md (client.RULES) travel in the tool descriptions and above others' text in what
the tools return. Imported at discovery (``provides_tools``), so it stays light.
"""

from __future__ import annotations

import logging
from typing import Any, Optional

from . import client, waiting

logger = logging.getLogger(__name__)

# The person each token speaks for (`you` of the inbox or of GET /api/people), by client.Settings.identity.
_me: dict[str, str] = {}


def _api() -> client.Api:
    settings = client.resolve_settings()
    if not settings.configured:
        raise client.RelevoError(0, "NOT_CONFIGURED", "set RELEVO_URL and RELEVO_TOKEN (or run `relevo connect`)")
    return client.Api(settings)


def _learn(api: client.Api, data: dict) -> None:
    me = client.you_of(data)
    if me:
        _me[api.settings.identity] = me


def _me_for(api: client.Api) -> str | None:
    """Who this token speaks for; asks the server once when no inbox has said it yet."""
    key = api.settings.identity
    if key not in _me:
        try:
            _learn(api, api.people())
        except Exception as exc:
            logger.warning("relevo: could not read who this token speaks for: %s", client.describe_error(exc))
    return _me.get(key)


def _run(name: str, fn, api: Optional[client.Api] = None) -> str:
    try:
        return fn()
    except client.RelevoError as exc:
        return "Error: " + client.describe_refusal(exc, _me.get(api.settings.identity) if api else None)
    except client.CertError as exc:
        # Nothing was sent: the certificate to trust cannot be used, and no other is tried.
        return f"Error: {exc}."
    except Exception as exc:
        logger.warning("relevo: %s failed: %s", name, client.describe_error(exc))
        return f"Error: could not reach the Relevo server ({client.describe_error(exc)})."


# relevo_send's fields: `done` and `close` are the deprecated names (0.4.x) read as `seen`. Others are
# refused, so a misnamed one (`message`, `recipients`, the old `to`) fails with the list of the right
# ones instead of being dropped.
_SEND_FIELDS = ("text", "thread", "bound", "claim", "seen", "done", "close")


def relevo_send(args: dict, session_id: Optional[str] = None, **_: Any) -> str:
    unknown = sorted(k for k in args if k not in _SEND_FIELDS)
    if unknown:
        return ("Error: relevo_send takes text, thread, claim and seen (and, deprecated, done, and close "
                f"without text, read as seen); not {', '.join(unknown)}. Nothing was sent. "
                "Who a note is for is written in its text: @handle for a person, @team-name for a team, no @ "
                "for the global board.")

    def go() -> str:
        payload: dict[str, Any] = {}
        # The key whenever it was given, empty string included: an empty `thread` is no thread, and
        # the server is the one that says so (`relevo mcp` and pi pass it through the same way).
        # Dropping it here would open a new thread, and a text with no @ would reach everyone.
        thread = args.get("thread")
        bound = args.get("bound")
        if bound is not None and not waiting.numbers(thread):
            return "Error: `bound` goes only with list numbers in `thread`, as /relevo gave them. Nothing was sent."
        if thread is not None:
            payload["thread"] = str(thread)
        if args.get("text") is not None:
            payload["text"] = str(args["text"])
        seen, done, claim = bool(args.get("seen")), bool(args.get("done")), bool(args.get("claim"))
        # Threads no longer close: a close without text is `seen` (and claims nothing, as it never
        # did); with text it is only the note. It never goes to the server, which up to 0.4.x would
        # close the thread.
        if args.get("close") and "text" not in payload and not (seen or done):
            seen, claim = True, False
        if claim:
            payload["claim"] = True
        # `seen` goes on the wire as `done`, its name up to 0.4.x, which servers take through 0.x: a
        # 0.4.x server refuses `seen`, and the plugin may be newer than its server. Both given, both
        # go, for the server to refuse.
        if seen and done:
            payload["seen"] = payload["done"] = True
        elif seen or done:
            payload["done"] = True
        ns = waiting.numbers(thread)
        if ns:
            # Numbers of the person's last /relevo list in this Hermes session, checked first.
            del payload["thread"]
            return waiting.send_listed(api, waiting.tool_keys(session_id), ns, payload, seen or done,
                                       lambda result: client.describe_send(result), bound)
        if seen or done:
            return client.describe_seen(api.send(payload))
        return client.describe_send(api.send(payload), opened=thread is None)

    try:
        api = _api()
    except client.RelevoError as exc:
        return "Error: " + client.describe_refusal(exc)
    return _run("relevo_send", go, api)


def relevo_inbox(args: dict, **_: Any) -> str:
    def go() -> str:
        api = _api()
        data = api.inbox(all_=bool(args.get("all")))
        _learn(api, data)
        return client.render_inbox(data, _me_for(api))

    return _run("relevo_inbox", go)


def relevo_thread(args: dict, session_id: Optional[str] = None, **_: Any) -> str:
    def go() -> str:
        api = _api()
        thread_id = str(args.get("id") or "").strip()
        if not thread_id:
            return "Error: give the thread id (r_…)."
        head = ""
        ns = waiting.numbers(thread_id)
        if ns:
            found = waiting.thread_head(waiting.tool_keys(session_id), ns, args.get("bound"))
            if isinstance(found, str):
                return found
            thread_id, head = found
        data = api.thread(thread_id)
        if head and data.get("pending") is not True:
            head += "It is no longer waiting on your person: they handled it since the list.\n"
        return head + client.render_thread(data, _me_for(api))

    return _run("relevo_thread", go)


_TOOLS: dict[str, tuple[Any, str, dict, list[str]]] = {
    "relevo_send": (
        relevo_send,
        "Send a Relevo note: open a thread (no `thread`) or write in one (`thread`). Fields: `text` (the "
        "note), `thread` (an existing thread's id, \"r_…\", or a number of the person's last /relevo "
        "list), `claim` (true, with `thread`, to mark the "
        "thread as this session's), `seen` (true, with `thread` and no text, to mark it seen when it needs "
        "nothing: an answer you have now read, a thank-you, an answer that settles what you asked); `done`, "
        "and `close` without text, deprecated, are read as `seen`. Who the "
        "note is for is written in its text: @handle for a person (@handle/repo or @handle/repo@branch for "
        "one context), @team-name for a team, and no @ at all to post on the global board, which everyone "
        "on the server sees and nobody has to answer: use it for announcements, or when nobody knows who "
        "knows and your person agrees. "
        "Every @ outside code sends the note to that person or team, so write a name without @ when only "
        "talking about someone, and put code, decorators, npm scopes and emails in backticks. In a reply, "
        "an @person names them (it becomes pending for them) and adds them if they were not in the thread; "
        "a team cannot be mentioned in a reply. If your person names whom to ask, send it there. "
        "Otherwise, before opening one, look at your inbox (relevo_inbox: an answer may already be "
        "waiting): name the person whose `about` line fits; name a team when it is a team's matter; use "
        "the board (no @) only when nobody fits. Writing in a thread claims it for this session unless "
        "another of your person's sessions has it (then step aside); opening one claims nothing. Read what "
        "you need first; if you can answer or explain from that, just answer: the answer claims it. Answer "
        "only from your own work in this session, the folder you work in and what your person told you; never "
        "look outside that folder for an answer (their home, other folders, mail, shell history, crontab, "
        "system files): if that is not enough, leave the note pending. If the "
        "note needs work, claim the thread with `thread`, `claim: true` and no text just before you start "
        "changing files or running something long: a state the others see, not a note. Never write a note "
        "that only says you are on it. Do not claim a team or global-board thread that does not name your "
        "person; answer one only from your own work or the folder you work in. The note must be "
        "self-contained: the reader does not share your context. Write in the first person, as your "
        "person would (\"I changed…\"). A new thread starts with a one-line subject (its title), no "
        "greeting and no \"Re:\". Threads do not close: when you asked and the answer settles it, mark it "
        "`seen`. `seen: true` with `thread` and no text writes nothing, and this session lets go of the "
        "thread if it held it. A note stays pending for your person until they answer it or mark it `seen`, "
        "and is pending again when someone writes after; if it is not for you, write nothing and leave it "
        "pending. The answer says whom the note "
        "reached (check it is who your person meant) and who has the thread.\n\n"
        + client.TOOL_RULES,
        {
            "text": {"type": "string", "description": "The note (Markdown). Who it is for is written in it: "
                                                      "@handle for a person (@handle/repo or "
                                                      "@handle/repo@branch for one context), @team-name for a "
                                                      "team, no @ for the global board. Every @ outside code sends it there: put "
                                                      "code, decorators, npm scopes and emails in backticks. "
                                                      "In the first person, as your "
                                                      "person would write it (\"I changed…\", not \"Ana "
                                                      "changed…\"). Opening a thread, its first line is a "
                                                      "one-line subject (the title), no greeting and no "
                                                      "\"Re:\". Optional only when claiming or marking "
                                                      "seen. "
                                                      "Never a note that only says you are on it."},
            "thread": {"type": "string", "description": "Thread id (r_…) to write in, or its number in the "
                                                        "person's last /relevo list as they typed it (\"1\"; with "
                                                        "`seen: true` and no text, several: \"1 3\"): the tool "
                                                        "reads the thread first and sends nothing if it was "
                                                        "handled or got a newer note since. Never guess an id. "
                                                        "Without it, `text` opens "
                                                        "a new thread, for whom its @ mentions name (none: the "
                                                        "global board)."},
            "bound": {"type": "string", "description": "With list numbers in `thread`: `bound` as a /relevo "
                                                       "request gives it (\"1=r_…:n_…\"), what each number stood "
                                                       "for when the person typed it; the tool refuses when the "
                                                       "list changed since. Copy it as it is; never make one up."},
            "claim": {"type": "boolean",
                      "description": "With `thread`: marks the thread as this session's, a "
                                     "state the others in it see (\"has it since 10:47\"), without "
                                     "writing a note when there is no `text`. Use it just before you "
                                     "start changing files or running something long. Not for a team or "
                                     "global-board thread that does not name your person."},
            "seen": {"type": "boolean",
                     "description": "With `thread` and no `text` (and no `claim`): your person has seen the "
                                    "thread up to the latest note this session has received and has nothing "
                                    "to add (an answer you have now read, a thank-you, an answer that settles "
                                    "what you asked). A note that arrived since stays pending. It stops being "
                                    "pending for them until someone writes again; nothing is written, and "
                                    "this session lets go of the thread if it held it. Threads do not close. "
                                    "Not for a note that is not for you: leave that one pending."},
            "done": {"type": "boolean",
                     "description": "Deprecated: the name of `seen` up to relevo 0.4. The same as `seen`; "
                                    "not with it."},
            "close": {"type": "boolean",
                      "description": "Deprecated: threads no longer close. Without `text` it is read as "
                                     "`seen`; with `text`, ignored (writing the note handles the thread)."},
        },
        [],
    ),
    "relevo_inbox": (
        relevo_inbox,
        "What is new in Relevo for this session and what is pending for your person, with who you "
        "speak for (`you`), the people (handle, what they look after, the teams they are in, live "
        "contexts) and the teams (name, title, members), to mention with @. Look here before opening a "
        "thread: an answer may already be waiting.\n\n" + client.TOOL_RULES,
        {"all": {"type": "boolean",
                 "description": "Every thread of your person, whichever session it is for."}},
        [],
    ),
    "relevo_thread": (
        relevo_thread,
        "A whole Relevo thread: every note in full, who has seen it, who has handled it and who holds it.",
        {"id": {"type": "string", "description": "Thread id (r_…), or its number in the person's last /relevo "
                                                 "list (\"2\")."},
         "bound": {"type": "string", "description": "With a list number: `bound` as a /relevo request gives it "
                                                    "(\"2=r_…:n_…\"), what the number stood for when the person "
                                                    "typed it. Copy it as it is; never make one up."}},
        ["id"],
    ),
}


def register_tools(ctx) -> None:
    for name, (handler, description, properties, required) in _TOOLS.items():
        parameters: dict[str, Any] = {"type": "object", "properties": properties}
        if required:
            parameters["required"] = required
        ctx.register_tool(
            name=name, toolset="relevo", handler=handler, description=description,
            schema={"name": name, "description": description, "parameters": parameters},
            check_fn=client.configured, emoji="\U0001f4e8",
        )
