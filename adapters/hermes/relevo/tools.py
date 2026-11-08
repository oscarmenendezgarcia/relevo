"""Relevo client tools (``relevo`` toolset): relevo_send, relevo_inbox, relevo_thread.

Same behaviour as the `relevo mcp` tools of docs/agents.md, calling the HTTP API directly (for the
session client.session_id picks). Sessions that have only these tools never see the relevo platform
hint, so the rules of docs/agents.md (client.RULES) travel in the tool descriptions and above others' text in what
the tools return. Imported at discovery (``provides_tools``), so it stays light.
"""

from __future__ import annotations

import logging
from typing import Any

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


def _run(name: str, fn) -> str:
    try:
        return fn()
    except client.RelevoError as exc:
        return "Error: " + client.describe_refusal(exc)
    except Exception as exc:
        logger.warning("relevo: %s failed: %s", name, client.describe_error(exc))
        return f"Error: could not reach the Relevo server ({client.describe_error(exc)})."


def relevo_send(args: dict, **_: Any) -> str:
    def go() -> str:
        payload: dict[str, Any] = {}
        to, thread = args.get("to"), args.get("thread")
        if isinstance(to, str):
            to = [to]
        if to:
            payload["to"] = list(to)
        if thread:
            payload["thread"] = str(thread)
        if args.get("text") is not None:
            payload["text"] = str(args["text"])
        if args.get("close"):
            payload["close"] = True
        return client.describe_send(_api().send(payload))

    return _run("relevo_send", go)


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
        "Open a Relevo thread or write in one. Use `to` (handles such as \"ana\", \"ana/pagos-api\", "
        "\"ana/pagos-api@feat/refunds\", or [\"@team\"] for a question to everyone) to open a new thread, "
        "or `thread` to write in an existing one. If your person names whom to ask (a teammate, or the "
        "whole team: `@team`), send it there. Otherwise, before opening one, look at your inbox "
        "(relevo_inbox: an answer may already be waiting) and use the team's `about` lines to choose "
        "whom to ask; `@team` only when you do not know who knows. If you can answer at once, just "
        "answer: no \"On it.\" first. Send \"On it.\" only when the note needs work before you can "
        "answer, and never to a team question; answer a team question only from your own work or your "
        "person's files. Says who holds the thread.\n\n" + client.TOOL_RULES,
        {
            "to": {"type": "array", "items": {"type": "string"},
                   "description": "Addresses for a new thread, or [\"@team\"] (when your person asks the "
                                  "whole team, or you do not know who knows). Not with `thread`."},
            "thread": {"type": "string", "description": "Thread id (r_…) to write in. Not with `to`."},
            "text": {"type": "string", "description": "The note (Markdown). Optional only when closing."},
            "close": {"type": "boolean", "description": "Close the thread after writing."},
        },
        [],
    ),
    "relevo_inbox": (
        relevo_inbox,
        "What is new in Relevo for this session and what waits for your person's reply, with who you "
        "speak for (`you`) and the team list (handle, what they look after, live contexts). Look here "
        "before opening a thread: an answer may already be waiting.\n\n" + client.TOOL_RULES,
        {"all": {"type": "boolean",
                 "description": "Every thread of your person, whichever session it is for."}},
        [],
    ),
    "relevo_thread": (
        relevo_thread,
        "A whole Relevo thread: every note in full, who has seen it and who holds it.",
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
