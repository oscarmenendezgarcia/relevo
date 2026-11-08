"""Relevo platform adapter: every Relevo thread is a chat, every new note an inbound message.

connect() keeps one waiting inbox request open (wait=55) and backs off on errors; each thread with
new notes becomes a MessageEvent from the author of its latest note; send() writes the agent's reply
in that thread. One fixed Relevo-Session per profile, general unless RELEVO_CONTEXT is set.
"""

from __future__ import annotations

import asyncio
import logging
from collections import OrderedDict
from typing import Any, Optional

from gateway.config import Platform, PlatformConfig
from gateway.platforms.base import BasePlatformAdapter, SendResult
from gateway.platforms.event import MessageEvent, MessageType

from . import client

logger = logging.getLogger(__name__)

PLATFORM_NAME = "relevo"
# The gateway's own busy acknowledgements (interrupting, queued, steered, saved): chat chrome that a
# teammate reading the thread has no use for.
_GATEWAY_ACK_PREFIXES = ("⚡", "⏳", "⏩", "\U0001f4be")
_SEEN_LIMIT = 2048
_ERROR_KINDS = {
    "NOT_FOUND": "not_found", "THREAD_CLOSED": "forbidden", "FORBIDDEN": "forbidden",
    "TOO_LARGE": "too_long", "THREAD_BUSY": "rate_limited", "RATE_LIMITED": "rate_limited",
}


class RelevoAdapter(BasePlatformAdapter):
    wait_seconds = client.WAIT_SECONDS
    backoff_initial = 1.0
    backoff_max = 60.0

    def __init__(self, config: PlatformConfig):
        super().__init__(config, Platform(PLATFORM_NAME))
        self.settings = client.resolve_settings(config.extra or {})
        self.api = client.Api(self.settings)
        self._http: Any = None
        self._poll_task: Optional[asyncio.Task] = None
        self._stop: Optional[asyncio.Event] = None
        self._seen: OrderedDict[str, None] = OrderedDict()
        self._titles: dict[str, str] = {}
        self._me: Optional[str] = None

    # -- lifecycle ------------------------------------------------------------------------------

    async def connect(self, *, is_reconnect: bool = False) -> bool:
        if not self.settings.configured:
            self._set_fatal_error("relevo_not_configured",
                                  "Relevo needs RELEVO_URL and RELEVO_TOKEN", retryable=False)
            return False
        if self._poll_task and not self._poll_task.done():
            return True
        self._ensure_http()
        self._stop = asyncio.Event()
        self._poll_task = asyncio.create_task(self._poll_loop(), name="relevo-inbox")
        self._mark_connected()
        logger.info("[relevo] connected; session %s, context %s", self.settings.session,
                    self.settings.context or "general")
        return True

    async def disconnect(self) -> None:
        if self._stop is not None:
            self._stop.set()
        task, self._poll_task = self._poll_task, None
        if task is not None and task is not asyncio.current_task():
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
        http, self._http = self._http, None
        if http is not None:
            await http.aclose()
        self._mark_disconnected()

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
                if exc.status == 401:
                    logger.error("[relevo] the server refused the token (%s); stopping",
                                 client.describe_error(exc))
                    self._set_fatal_error("relevo_unauthorized",
                                          "Relevo refused the token: it is unknown or revoked",
                                          retryable=False)
                    await self._notify_fatal_error()
                    return
                logger.warning("[relevo] inbox failed: %s; retrying in %.0fs", client.describe_error(exc), delay)
            except Exception as exc:
                logger.warning("[relevo] inbox failed: %s; retrying in %.0fs", client.describe_error(exc), delay)
            else:
                delay = self.backoff_initial
                team = {p.get("handle"): p for p in data.get("team") or []}
                for thread in data.get("threads") or []:
                    try:
                        await self._dispatch(thread, team)
                    except asyncio.CancelledError:
                        raise
                    except Exception as exc:
                        logger.warning("[relevo] could not hand over thread %s: %s", thread.get("id"),
                                       client.describe_error(exc))
                continue
            if await self._pause(delay):
                return
            delay = min(delay * 2, self.backoff_max)

    # -- inbound --------------------------------------------------------------------------------

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
        self._me = client.learn_self(thread, notes) or self._me
        if any(n.get("truncated") for n in notes):
            full = {n.get("id"): n for n in (await self.api.athread(self._ensure_http(), thread_id)).get("notes") or []}
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
        if content.startswith(_GATEWAY_ACK_PREFIXES):
            return SendResult(success=True)
        if not content.strip():
            return SendResult(success=False, error="empty note", error_kind="unknown")
        try:
            result = await self.api.asend(self._ensure_http(), {"thread": str(chat_id), "text": content})
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

    async def get_chat_info(self, chat_id: str) -> dict[str, Any]:
        return {"name": self._titles.get(str(chat_id), str(chat_id)), "type": "dm"}
