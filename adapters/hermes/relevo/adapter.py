"""Relevo platform adapter: every Relevo thread is a chat, every new note an inbound message.

connect() keeps one waiting inbox request open (wait=55) and backs off on errors; each thread with
new notes becomes a MessageEvent from the author of its latest note; send() writes the agent's reply
in that thread; disconnect() stops the loop, then ends the session with DELETE /api/session. One
fixed Relevo-Session per profile (see client.gateway_session_id), general unless
RELEVO_CONTEXT is set.
"""

from __future__ import annotations

import asyncio
import logging
import re
import sys
from collections import OrderedDict
from dataclasses import dataclass
from typing import Any, Optional

from gateway.config import Platform, PlatformConfig
from gateway.platforms.base import BasePlatformAdapter, SendResult
from gateway.platforms.event import MessageEvent, MessageType

from . import client

logger = logging.getLogger(__name__)

PLATFORM_NAME = "relevo"
# The one silence marker. The gateway already drops a bare one when reply_expected is False.
_SILENCE = "[SILENT]"
# The failure a send that is not a turn's final reply gets: nothing was written, nothing to retry.
_KEPT_OUT = "kept out of the Relevo thread: only a turn's final reply becomes a note"
_SEEN_LIMIT = 2048
_ERROR_KINDS = {
    "NOT_FOUND": "not_found", "THREAD_CLOSED": "forbidden", "FORBIDDEN": "forbidden",
    "TOO_LARGE": "too_long", "THREAD_BUSY": "rate_limited", "RATE_LIMITED": "rate_limited",
}


# -- the gateway's own copy -------------------------------------------------------------------------
#
# When a turn fails, the gateway's final reply is its own text, marked final like an answer. The real
# GatewayRunner composes it: error copy (gateway.errors.* in the catalog, or the tables of
# agent/turn_failure_copy.py), often with a "Details: …" or "Provider said: …" paragraph, then the
# runtime footer when the owner turned it on, then the failed-turn notice (FAILED_TURN_NOTICE or
# PARTIAL_FAILED_TURN_NOTICE), each after a blank line. Teammates must read none of it: it leaks
# details, and a note would claim the thread for a session that could not answer.

_DIAGNOSTIC_KEY_PREFIXES = ("gateway.errors.",)
_DIAGNOSTIC_KEYS = ("gateway.notify.turn_error", "gateway.notify.turn_error_detailed",
                    "gateway.notify.delivery_failed_retry")
# Appended to a reply after an automatic session reset: the gateway's, not the agent's.
_TRAILER_KEYS = ("gateway.session.auto_reset_context_exhausted",)
# turn_failure_copy's chat copy tables (English only), read when this Hermes has them.
_COPY_TABLES = ("_SITE_COPY", "_NONRETRYABLE_COPY", "_NONRETRYABLE_DEFAULT_COPY", "_AUTH_COPY")
_FAILED_TURN_NOTICES = ("FAILED_TURN_NOTICE", "PARTIAL_FAILED_TURN_NOTICE", "AUTOMATION_INTERRUPTED_NOTICE")
# The same notices, for a Hermes without agent.turn_failure_copy.
_FALLBACK_NOTICES = (
    "Your request was not processed. Send it again if you still want me to carry it out.",
    "This turn did not complete. Some actions may already have run; verify their effects before resending.",
)
# A template needs this many characters besides its placeholders to say anything: shorter entries
# are fragments of the copy above ("The model"), not replies.
_DIAGNOSTIC_MIN_CHARS = 24
_PARAGRAPH_BREAK = re.compile(r"\n[ \t]*\n\s*")
_PLACEHOLDER = re.compile(r"\{[^{}]*\}")
# The raw detail the copy carries last: "Details: launch_credentials_unavailable (provider: …)".
_DETAIL_PARAGRAPH = re.compile(r"(?:Details|Provider said):\s")
# The runtime footer (gateway/runtime_footer.py): model, served model, context %, latency, cwd.
_FOOTER_SEP = " · "
_FOOTER_MODEL = re.compile(r"[\w.:@+/-]+(?: → [\w.:@+/-]+)?")
_FOOTER_FACT = re.compile(r"\d{1,3}%|<1s|\d+s|\d+m\d{2}s|~(?:/.*)?|/.*")


@dataclass(frozen=True)
class _GatewayCopy:
    patterns: tuple
    notices: frozenset
    trailers: frozenset


_copy_cache: dict[str, _GatewayCopy] = {}


def _squash(text: str) -> str:
    return " ".join(str(text).split())


def _split_last(text: str) -> tuple[str, str]:
    """(everything before the last paragraph, the last paragraph)."""
    breaks = list(_PARAGRAPH_BREAK.finditer(text))
    if not breaks:
        return "", text
    return text[:breaks[-1].start()], text[breaks[-1].end():]


def _without_details(text: str) -> str:
    head, last = _split_last(text.strip())
    while head and _DETAIL_PARAGRAPH.match(last):
        head, last = _split_last(head.rstrip())
    return f"{head}\n\n{last}" if head else last


def _template_pattern(template: str) -> Optional[re.Pattern]:
    literal = _squash(_without_details(template))
    if len(_PLACEHOLDER.sub("", literal)) < _DIAGNOSTIC_MIN_CHARS:
        return None
    return re.compile(".*?".join(re.escape(part) for part in _PLACEHOLDER.split(literal)), re.S)


def _gateway_copy() -> _GatewayCopy:
    """The gateway's failure copy in the active language and English, and the notices it appends."""
    try:
        from agent.i18n import get_language

        lang = get_language()
    except Exception:
        lang = "en"
    cached = _copy_cache.get(lang)
    if cached is not None:
        return cached
    templates: set[str] = set()
    trailers: set[str] = set()
    try:
        from agent.i18n import _load_catalog

        for code in {lang, "en"}:
            for key, value in _load_catalog(code).items():
                if key.startswith(_DIAGNOSTIC_KEY_PREFIXES) or key in _DIAGNOSTIC_KEYS:
                    templates.add(str(value))
                elif key in _TRAILER_KEYS:
                    trailers.add(_squash(value))
    except Exception:
        try:
            from agent.i18n import t

            templates.update(t(key) for key in _DIAGNOSTIC_KEYS)
            trailers.update(_squash(t(key)) for key in _TRAILER_KEYS)
        except Exception:
            pass
    notices = {_squash(n) for n in _FALLBACK_NOTICES}
    try:
        from agent import turn_failure_copy as copy

        notices.update(_squash(getattr(copy, name)) for name in _FAILED_TURN_NOTICES if hasattr(copy, name))
        for name in _COPY_TABLES:
            table = getattr(copy, name, None)
            templates.update(map(str, table.values()) if isinstance(table, dict) else [str(table or "")])
    except Exception:
        pass
    patterns = tuple(p for p in map(_template_pattern, templates) if p is not None)
    found = _GatewayCopy(patterns, frozenset(notices), frozenset(t for t in trailers if t))
    _copy_cache[lang] = found
    return found


def _footer_enabled() -> bool:
    """Whether the owner turned the runtime footer on for this platform (unknown: assume so). Reads
    the config the way the gateway does, through gateway.run when it is loaded (always, in the
    gateway): importing it runs Hermes' launch bootstrap, so this never does."""
    try:
        from gateway.runtime_footer import resolve_footer_config

        run = sys.modules.get("gateway.run")
        if run is not None:
            config = run._load_gateway_config()
        else:
            from hermes_cli.config_effective import load_user_config_effective
            from hermes_constants import get_hermes_home

            config = load_user_config_effective(get_hermes_home() / "config.yaml")
        return bool(resolve_footer_config(config, PLATFORM_NAME).get("enabled"))
    except Exception:
        return True


def _is_footer(paragraph: str) -> bool:
    parts = paragraph.strip().split(_FOOTER_SEP)
    return ("\n" not in paragraph.strip() and 2 <= len(parts) <= 5
            and all(_FOOTER_MODEL.fullmatch(p) or _FOOTER_FACT.fullmatch(p) for p in parts)
            and any(_FOOTER_FACT.fullmatch(p) for p in parts))


def note_text(content: str) -> Optional[str]:
    """The note a final reply becomes, or None when it is the gateway's copy for a failed turn.

    The gateway's trailing paragraphs come off from the end: a failed-turn notice (then the whole
    reply is the gateway's: error copy, or a half-done answer the gateway says did not complete), the
    runtime footer (the model and the owner's working directory are not for teammates) and the
    automatic-reset notice. What is left is the gateway's when it is its error copy, with or without
    a "Details:" paragraph."""
    copy = _gateway_copy()
    body = content.rstrip()
    footer = None
    while body:
        head, last = _split_last(body)
        squashed = _squash(last)
        if squashed in copy.notices:
            return None
        if squashed in copy.trailers:
            body = head.rstrip()
            continue
        if head.strip() and _is_footer(last):
            if footer is None:
                footer = _footer_enabled()
            if footer:
                body = head.rstrip()
                continue
        break
    if not body.strip():
        return None if content.strip() else content
    error = _squash(_without_details(body))
    if any(p.fullmatch(error) for p in copy.patterns):
        return None
    return content if body == content.rstrip() else body


def is_gateway_diagnostic(content: str) -> bool:
    return note_text(content) is None


# The adapters of this process between connect() and disconnect(). The gateway may connect a new
# adapter before it disconnects the one it replaces; both have the profile's one session, which the
# old one must then leave to the new one rather than end it.
_LIVE: set["RelevoAdapter"] = set()


class _Stopping(Exception):
    """disconnect() was called while the loop waited on something it can do without."""


class RelevoAdapter(BasePlatformAdapter):
    # No edits on Relevo: a note is final. The gateway then streams nothing.
    SUPPORTS_MESSAGE_EDITING = False
    wait_seconds = client.WAIT_SECONDS
    backoff_initial = 1.0
    backoff_max = 60.0
    # How long disconnect() lets the loop finish handing over a batch the server already delivered.
    stop_grace = 5.0
    # How long disconnect() waits for DELETE /api/session, which releases the threads this session holds.
    end_timeout = 2.0

    def __init__(self, config: PlatformConfig):
        super().__init__(config, Platform(PLATFORM_NAME))
        self.settings = client.resolve_settings(config.extra or {}, session=client.gateway_session_id())
        self.api = client.Api(self.settings)
        self._http: Any = None
        self._poll_task: Optional[asyncio.Task] = None
        self._stop: Optional[asyncio.Event] = None
        self._seen: OrderedDict[str, None] = OrderedDict()
        self._titles: dict[str, str] = {}
        self._me: Optional[str] = None
        # True while the loop hands over threads the server has already delivered to this session.
        self._holding_batch = False

    # -- lifecycle ------------------------------------------------------------------------------

    async def connect(self, *, is_reconnect: bool = False) -> bool:
        unpaired = self.settings.unpaired
        if unpaired:
            # Connecting with the saved pair would talk to another server than the one configured.
            self._set_fatal_error(
                "relevo_unpaired", f"{unpaired} is set without {client.partner(unpaired)}: set both (env or "
                f"config.yaml), or neither to use the server saved by `relevo connect`", retryable=False)
            return False
        if not self.settings.configured:
            self._set_fatal_error("relevo_not_configured",
                                  "Relevo needs RELEVO_URL and RELEVO_TOKEN", retryable=False)
            return False
        problem = client.context_problem(self.settings.context)
        if problem:
            # Every inbox would be refused with BAD_REQUEST: fail now instead of retrying forever.
            logger.error("[relevo] RELEVO_CONTEXT is not a valid context: %s", problem)
            self._set_fatal_error("relevo_bad_context", f"RELEVO_CONTEXT is not a valid context: {problem}",
                                  retryable=False)
            return False
        if self._poll_task and not self._poll_task.done():
            return True
        self._ensure_http()
        self._stop = asyncio.Event()
        self._poll_task = asyncio.create_task(self._poll_loop(), name="relevo-inbox")
        _LIVE.add(self)
        client.use_gateway_session(self.settings.session)
        self._mark_connected()
        logger.info("[relevo] connected; server and token from %s, session %s, context %s", self.settings.source,
                    self.settings.session, self.settings.context or "general")
        return True

    async def disconnect(self) -> None:
        _LIVE.discard(self)
        # While another live adapter has this session (one the gateway connected in this one's
        # place), the session is its: the tools keep acting for it, and it is not ended.
        if not self._shared():
            client.use_gateway_session(None)
        if self._stop is not None:
            self._stop.set()
        task, self._poll_task = self._poll_task, None
        if task is not None and task is not asyncio.current_task():
            if self._holding_batch and not task.done():
                # Mid-batch: the server already delivered these threads to the fixed session, so
                # they would never be new again. Let the loop hand them over, then it stops.
                await asyncio.wait({task}, timeout=self.stop_grace)
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
        http, self._http = self._http, None
        if task is not None and http is not None and not self._shared():
            await self._end_session(http)
        if http is not None:
            await http.aclose()
        self._mark_disconnected()

    def _shared(self) -> bool:
        """Another live adapter of this process has the same session on the same server and token."""
        mine = (self.settings.identity, self.settings.session)
        return any((a.settings.identity, a.settings.session) == mine for a in _LIVE)

    async def _end_session(self, http: Any) -> None:
        """Once the loop has stopped, end the session (DELETE /api/session), so the threads it holds
        show again in the person's other sessions now rather than when its claims lapse. Best effort:
        a server that cannot be reached in end_timeout only leaves the claims to lapse."""
        try:
            await self.api.aend_session(http, timeout=self.end_timeout)
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            logger.info("[relevo] could not end session %s: %s; its claims lapse in 15 minutes",
                        self.settings.session, client.describe_error(exc))
        else:
            logger.info("[relevo] ended session %s", self.settings.session)

    def _ensure_http(self) -> Any:
        if self._http is None:
            import httpx

            self._http = httpx.AsyncClient()
        return self._http

    async def _pause(self, seconds: float) -> bool:
        """Sleep, waking early on disconnect. True when the loop should stop."""
        assert self._stop is not None
        try:
            await asyncio.wait_for(self._stop.wait(), timeout=seconds)
        except asyncio.TimeoutError:
            return False
        return True

    async def _poll_loop(self) -> None:
        delay = self.backoff_initial
        while self._stop is not None and not self._stop.is_set():
            try:
                data = await self.api.ainbox(self._ensure_http(), wait=self.wait_seconds)
            except asyncio.CancelledError:
                raise
            except client.RelevoError as exc:
                if exc.status in (400, 401):
                    # A refused token, or headers the server calls malformed: asking again cannot help.
                    unauthorized = exc.status == 401
                    logger.error("[relevo] the server refused %s (%s); stopping",
                                 "the token" if unauthorized else "the request", client.describe_error(exc))
                    self._set_fatal_error(
                        "relevo_unauthorized" if unauthorized else "relevo_bad_request",
                        "Relevo refused the token: it is unknown or revoked" if unauthorized
                        else f"Relevo refused the inbox request: {exc.code}: {exc.message}",
                        retryable=False)
                    await self._notify_fatal_error()
                    return
                logger.warning("[relevo] inbox failed: %s; retrying in %.1fs", client.describe_error(exc), delay)
            except Exception as exc:
                logger.warning("[relevo] inbox failed: %s; retrying in %.1fs", client.describe_error(exc), delay)
            else:
                delay = self.backoff_initial
                self._me = client.you_of(data) or self._me
                team = {p.get("handle"): p for p in data.get("team") or []}
                self._holding_batch = True
                try:
                    for thread in data.get("threads") or []:
                        try:
                            await self._dispatch(thread, team)
                        except asyncio.CancelledError:
                            raise
                        except Exception as exc:
                            logger.warning("[relevo] could not hand over thread %s: %s", thread.get("id"),
                                           client.describe_error(exc))
                finally:
                    self._holding_batch = False
                continue
            if await self._pause(delay):
                return
            delay = min(delay * 2, self.backoff_max)

    # -- inbound --------------------------------------------------------------------------------

    async def _unless_stopping(self, coro: Any) -> Any:
        """Await coro, giving up (_Stopping) as soon as disconnect() is called: the batch the loop
        holds must reach handle_message before the task is cancelled."""
        work = asyncio.ensure_future(coro)
        if self._stop is None:
            return await work
        stop = asyncio.ensure_future(self._stop.wait())
        try:
            await asyncio.wait({work, stop}, return_when=asyncio.FIRST_COMPLETED)
        finally:
            stop.cancel()
        if not work.done():
            work.cancel()
            await asyncio.gather(work, return_exceptions=True)
            raise _Stopping()
        return work.result()

    def _remember(self, note_id: str) -> None:
        self._seen[note_id] = None
        while len(self._seen) > _SEEN_LIMIT:
            self._seen.popitem(last=False)

    async def _dispatch(self, thread: dict, team: dict) -> None:
        thread_id = str(thread.get("id") or "")
        notes = [n for n in thread.get("new_notes") or [] if n.get("id") not in self._seen]
        if not thread_id or not notes:
            return  # only waiting: it was handed over when its notes were new
        self._titles[thread_id] = str(thread.get("title") or thread_id)
        if any(n.get("truncated") for n in notes):
            try:
                whole = await self._unless_stopping(self.api.athread(self._ensure_http(), thread_id))
            except _Stopping:
                logger.info("[relevo] stopping: thread %s is handed over with its notes cut", thread_id)
            except Exception as exc:
                # The inbox already delivered these notes: hand them over cut rather than lose them.
                logger.warning("[relevo] could not read thread %s whole: %s", thread_id, client.describe_error(exc))
            else:
                full = {n.get("id"): n for n in whole.get("notes") or []}
                notes = [full.get(n.get("id"), n) for n in notes]
        for n in notes:
            self._remember(str(n.get("id")))
        others = [n for n in notes if n.get("author") != self._me]
        if not others:
            return  # only notes of our person's own sessions: nothing to answer
        latest = others[-1]
        author = str(latest.get("author") or "")
        person = team.get(author) or {}
        name = f"{person.get('name')} (@{author})" if person.get("name") else f"@{author}"
        source = self.build_source(chat_id=thread_id, chat_name=self._titles[thread_id], chat_type="dm",
                                   user_id=author, user_name=name)
        event = MessageEvent(
            text=client.render_event(thread, notes, self._me),
            message_type=MessageType.TEXT,
            source=source,
            user_id=author,
            user_name=name,
            raw_message=thread,
            message_id=str(latest.get("id")),
            # Others' text is conversation, never a gateway /command.
            allow_gateway_control=False,
            # A note may not be for this agent: a bare [SILENT] answer must stand.
            reply_expected=False,
        )
        logger.info("[relevo] %d new note(s) in thread %s", len(notes), thread_id)
        await self.handle_message(event)

    # -- outbound -------------------------------------------------------------------------------

    async def send(self, chat_id: str, content: str, reply_to: Optional[str] = None,
                   metadata: Optional[dict[str, Any]] = None) -> SendResult:
        """Write the agent's final reply in the thread, and nothing else. Teammates read every note,
        and each one claims the thread: tool progress, interim messages, busy acknowledgements,
        pairing codes and warnings stay out, whatever the owner's display settings say. The base
        adapter marks a final reply with metadata["notify"]; everything else goes without it, and
        a mid-turn send carries "_interim_send".

        What stays out is not delivered, and says so: the stream consumer counts a successful
        interim send as text the chat already has, and the gateway then skips a final reply that
        repeats it (the codex runtime delivers its final answer as commentary)."""
        meta = metadata or {}
        if not meta.get("notify") or meta.get("_interim_send"):
            logger.debug("[relevo] kept a gateway message out of thread %s: not a final reply", chat_id)
            return SendResult(success=False, error=_KEPT_OUT, error_kind="unknown")
        if content.strip() == _SILENCE:
            return SendResult(success=True)
        text = note_text(content)
        if text is None:
            logger.warning("[relevo] the turn for thread %s failed; the gateway's notice is kept out of the "
                           "thread (details in the gateway log)", chat_id)
            return SendResult(success=True)
        if not text.strip():
            return SendResult(success=False, error="empty note", error_kind="unknown")
        try:
            result = await self.api.asend(self._ensure_http(), {"thread": str(chat_id), "text": text})
        except client.RelevoError as exc:
            logger.warning("[relevo] send to %s refused: %s", chat_id, client.describe_error(exc))
            return SendResult(success=False, error=f"{exc.code}: {exc.message}",
                              error_kind=_ERROR_KINDS.get(exc.code, "unknown"))
        except Exception as exc:
            logger.warning("[relevo] send to %s failed: %s", chat_id, client.describe_error(exc))
            # Already retried once with the same key; a new attempt would get a new key.
            return SendResult(success=False, error=f"Relevo unreachable ({client.describe_error(exc)})",
                              error_kind="transient")
        if result.get("claimed") == "other-session":
            logger.info("[relevo] wrote in %s, held by another session of this person", chat_id)
        return SendResult(success=True, message_id=result.get("note"), raw_response=result)

    def _send_retry_is_final(self, result: SendResult) -> bool:
        """send() already retried once with the same key. Another attempt would carry a new key (a
        duplicate note if the first one landed), and the base class's plain-text fallback would write
        a copy with a gateway banner."""
        return True

    def warning_notifications_enabled(self, logical_platform=None, *, chat_id=None, metadata=None) -> bool:
        """Gateway diagnostics are for the owner's log, never for a teammate's thread."""
        return False

    async def get_chat_info(self, chat_id: str) -> dict[str, Any]:
        return {"name": self._titles.get(str(chat_id), str(chat_id)), "type": "dm"}
