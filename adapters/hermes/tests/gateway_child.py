"""The relevo gateway platform in a process of its own, for the test that kills it right after a note
reaches it (test_relevo.Sessions). It loads the plugin the way test_relevo does, from the Hermes home
in HERMES_HOME (the test's temporary one, where the plugin is installed already), and takes the
server and token from RELEVO_URL and RELEVO_TOKEN.

Prints `session <id>` once connected, then, for each thread handed over, claims it for the gateway's
session (as its agent does before longer work) and prints `received <thread id>`. It never answers:
it is killed mid-turn.
"""

from __future__ import annotations

import asyncio
import os
import sys
import types
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(os.environ.get("HERMES_SRC") or Path.home() / ".hermes" / "hermes-agent")))
# As in test_relevo: nothing may run Hermes' launch bootstrap.
sys.modules.setdefault("hermes_bootstrap", types.ModuleType("hermes_bootstrap"))

from gateway.config import PlatformConfig  # noqa: E402
from gateway.platform_registry import platform_registry  # noqa: E402
from hermes_cli.plugins import discover_plugins  # noqa: E402


def say(line: str) -> None:
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


async def main() -> None:
    discover_plugins()
    adapter = platform_registry.get("relevo").adapter_factory(PlatformConfig(enabled=True, extra={}))

    async def take(event) -> None:
        thread = event.source.chat_id
        await adapter.api.asend(adapter._ensure_http(), {"thread": thread, "claim": True})
        say(f"received {thread}")

    adapter.handle_message = take
    if not await adapter.connect():
        raise SystemExit("could not connect")
    say(f"session {adapter.settings.session}")
    await asyncio.Event().wait()


asyncio.run(main())
