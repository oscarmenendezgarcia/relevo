"""Relevo client tools (``relevo`` toolset): relevo_send, relevo_inbox, relevo_thread.

Same behaviour as the `relevo mcp` tools of docs/agents.md, calling the HTTP API directly (for the
session client.session_id picks). Sessions that have only these tools never see the relevo platform
hint, so the rules of docs/agents.md (client.RULES) travel in the tool descriptions and above others' text in what
the tools return. Imported at discovery (``provides_tools``), so it stays light.
"""

from __future__ import annotations

import logging
from typing import Any, Optional

from . import client

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


# relevo_send's fields. Others are refused, so a misnamed one (`message`, `recipients`, the old `to`)
# fails with the list of the right ones instead of being dropped.
_SEND_FIELDS = ("text", "thread", "close", "claim", "done")


def relevo_send(args: dict, **_: Any) -> str:
    unknown = sorted(k for k in args if k not in _SEND_FIELDS)
    if unknown:
        return (f"Error: relevo_send takes {', '.join(_SEND_FIELDS)}; not {', '.join(unknown)}. Nothing was sent. "
                "Who a note is for is written in its text: @handle for a person, @team-name for a team, no @ "
                "for the global board.")

    def go() -> str:
        payload: dict[str, Any] = {}
        thread = args.get("thread")
        if thread:
            payload["thread"] = str(thread)
        if args.get("text") is not None:
            payload["text"] = str(args["text"])
        if args.get("close"):
            payload["close"] = True
        if args.get("claim"):
            payload["claim"] = True
        if args.get("done"):
            payload["done"] = True
            return client.describe_done(api.send(payload))
        return client.describe_send(api.send(payload), opened=not thread)

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


def relevo_thread(args: dict, **_: Any) -> str:
    def go() -> str:
        api = _api()
        thread_id = str(args.get("id") or "").strip()
        if not thread_id:
            return "Error: give the thread id (r_…)."
        data = api.thread(thread_id)
        return client.render_thread(data, _me_for(api))

    return _run("relevo_thread", go)


_TOOLS: dict[str, tuple[Any, str, dict, list[str]]] = {
    "relevo_send": (
        relevo_send,
        "Send a Relevo note: open a thread (no `thread`) or write in one (`thread`). Fields: `text` (the "
        "note), `thread` (an existing thread's id, \"r_…\"), `close` (true to close the thread), `claim` "
        "(true, with `thread`, to mark the thread as this session's), `done` (true, with `thread` and no "
        "text, to mark it handled when it needs nothing: an answer you have now read, a thank-you). Who the "
        "note is for is written in its text: @handle for a person (@handle/repo or @handle/repo@branch for "
        "one context), @team-name for a team (@team for your person's own team when they are in exactly "
        "one), and no @ at all to post on the global board, which everyone on the server sees and nobody "
        "has to answer: use it for announcements, or when nobody knows who knows and your person agrees. "
        "Every @ outside code sends the note to that person or team, so write a name without @ when only "
        "talking about someone, and put code, decorators, npm scopes and emails in backticks. In a reply, "
        "an @person names them (it becomes pending for them) and adds them if they were not in the thread; "
        "a team cannot be mentioned in a reply. If your person names whom to ask, send it there. "
        "Otherwise, before opening one, look at your inbox (relevo_inbox: an answer may already be "
        "waiting): name the person whose `about` line fits; name a team when it is a team's matter; use "
        "the board (no @) only when nobody fits. Writing in a thread claims it for this session unless "
        "another of your person's sessions has it (then step aside); opening one claims nothing. Read what "
        "you need first; if you can answer or explain from that, just answer: the answer claims it. If the "
        "note needs work, claim the thread with `thread`, `claim: true` and no text just before you start "
        "changing files or running something long: a state the others see, not a note. Never write a note "
        "that only says you are on it. Do not claim a team or global-board thread that does not name your "
        "person; answer one only from your own work or your person's files. The note must be "
        "self-contained: the reader does not share your context. Write in the first person, as your "
        "person would (\"I changed…\"). A new thread starts with a one-line subject (its title), no "
        "greeting and no \"Re:\". `close: true` closes the thread after writing: only when your person "
        "asks, or when you asked the question and it is answered. `done: true` with `thread` and no text "
        "marks the thread handled when it needs nothing (an answer you have now read, a thank-you): nothing "
        "is written or claimed. A note stays pending for your person until they answer, close it or mark it "
        "done; if it is not for you, write nothing and leave it pending. The answer says whom the note "
        "reached (check it is who your person meant) and who has the thread.\n\n"
        + client.TOOL_RULES,
        {
            "text": {"type": "string", "description": "The note (Markdown). Who it is for is written in it: "
                                                      "@handle for a person (@handle/repo or "
                                                      "@handle/repo@branch for one context), @team-name for a "
                                                      "team (@team for your person's one team), no @ for the "
                                                      "global board. Every @ outside code sends it there: put "
                                                      "code, decorators, npm scopes and emails in backticks. "
                                                      "In the first person, as your "
                                                      "person would write it (\"I changed…\", not \"Ana "
                                                      "changed…\"). Opening a thread, its first line is a "
                                                      "one-line subject (the title), no greeting and no "
                                                      "\"Re:\". Optional only when closing, claiming or "
                                                      "marking done. "
                                                      "Never a note that only says you are on it."},
            "thread": {"type": "string", "description": "Thread id (r_…) to write in. Without it, `text` opens "
                                                        "a new thread, for whom its @ mentions name (none: the "
                                                        "global board)."},
            "close": {"type": "boolean", "description": "With `thread`: close the thread after writing. Only "
                                                        "when your person asks, or when you asked the question "
                                                        "and it is answered."},
            "claim": {"type": "boolean",
                      "description": "With `thread`: marks the thread as this session's, a "
                                     "state the others in it see (\"has it since 10:47\"), without "
                                     "writing a note when there is no `text`. Use it just before you "
                                     "start changing files or running something long. Not for a team or "
                                     "global-board thread that does not name your person."},
            "done": {"type": "boolean",
                     "description": "With `thread` and no `text` (and no `claim`): your person has handled "
                                    "the thread up to the latest note this session has received, with nothing "
                                    "to write (\"seen, nothing to do\": an answer you have now read, a "
                                    "thank-you). A note that arrived since stays pending. It stops being "
                                    "pending for them; nothing is written and nothing claimed. Not for a "
                                    "note that is not for you: leave that one pending."},
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
        {"id": {"type": "string", "description": "Thread id (r_…)."}},
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
