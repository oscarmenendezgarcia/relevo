"""The automatic update (docs/update.md): when the gateway platform connects, it runs
`relevo update --auto --adapter hermes --path <this plugin's folder>` in the background. The relevo
binary does the work (the signature, the checks, the swap, at most one check an hour per machine);
the plugin only starts it and logs what came of it.

Whether the gateway must be restarted does not depend on which client ran the update (a relevo mcp
or pi on this machine updates the plugin's folder as well, and usually first): at connect, and after
each update run, the plugin compares the version in its folder with the one this process loaded.
When they differ it tells the person once per version, in a Hermes home channel, and tries again a
few times while no home channel takes it. It never restarts anything: when to interrupt the
assistants is the owner's call.
"""

from __future__ import annotations

import asyncio
import logging
import os
import re
import subprocess
import threading
from pathlib import Path
from typing import Any, Awaitable, Callable, Optional

from . import client

logger = logging.getLogger(__name__)

# This plugin's own folder, which the update replaces (the real one, when it is reached by a link).
FOLDER = Path(__file__).resolve().parent
_OFF = {"0", "false", "no", "off"}
# One update at a time from this process: a reconnect while one runs starts none.
_running = threading.Lock()
# When no home channel took the restart notice (none connected yet, say), it is tried again after
# each of these delays, in seconds; and at the next connect.
RETRY_DELAYS: tuple = (10, 60, 300, 1800)


def plugin_version(folder: Path) -> Optional[str]:
    """The version plugin.yaml in `folder` says (its top-level `version:`), None when there is none."""
    try:
        text = (folder / "plugin.yaml").read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None
    for line in text.splitlines():
        if line.startswith("version:"):
            return line.split(":", 1)[1].strip().strip("\"'") or None
    return None


# The version this process loaded: another one in FOLDER runs only once the gateway restarts.
LOADED = plugin_version(FOLDER)
# The versions in the folder the person was told of (or is being told of right now).
_told: set = set()
_notices: set = set()
# The relevo binaries found to have no updater (before 0.4.0, where `update` is an unknown
# subcommand): said once, then not run again by this process, as the hourly throttle lives in the
# updater they lack.
_no_updater: set = set()


def has_no_updater(code: Optional[int], stderr: str) -> bool:
    """Whether relevo's exit says it has no `update` subcommand (clap's usage error)."""
    return code == 2 and re.search(r"unrecognized subcommand '?update'?", stderr) is not None

Tell = Callable[[str], Awaitable[Any]]


def _profile_value(name: str) -> str:
    """`name` from the profile's env (a secondary profile of a multiplexed gateway has its own
    .env), else ""."""
    try:
        from gateway.platforms._shared import get_scoped_secret

        return str(get_scoped_secret(name, "") or "")
    except Exception:
        return os.environ.get(name, "")


def auto_update_off() -> bool:
    """Whether RELEVO_AUTO_UPDATE turns the automatic update off, in the profile's env or the
    gateway's, as relevo reads it."""
    return any(v.strip().lower() in _OFF for v in (_profile_value("RELEVO_AUTO_UPDATE"),
                                                   os.environ.get("RELEVO_AUTO_UPDATE", "")))


def _runnable(path: Path) -> bool:
    return path.is_file() and os.access(path, os.X_OK)


def find_relevo() -> Optional[str]:
    """The relevo binary: RELEVO_BIN when set, else `relevo` on PATH; None when there is none."""
    named = _profile_value("RELEVO_BIN") or os.environ.get("RELEVO_BIN", "")
    if named:
        return named if _runnable(Path(named)) else None
    for entry in os.environ.get("PATH", "").split(os.pathsep):
        # An empty or relative entry would depend on the gateway's working directory.
        if entry and os.path.isabs(entry) and _runnable(Path(entry) / "relevo"):
            return str(Path(entry) / "relevo")
    return None


def in_git_checkout(folder: Path) -> bool:
    """Whether `folder` is in a git checkout: such a folder is updated with git, never replaced
    (the swap would throw away what git tracks there, uncommitted work included)."""
    return any((d / ".git").exists() for d in (folder, *folder.parents))


def child_env(settings: client.Settings) -> dict:
    """relevo's environment: the gateway's, with the profile's Relevo settings, so it updates
    from the server this platform talks to. A pair set in the profile's env or config.yaml is
    handed over as RELEVO_URL and RELEVO_TOKEN; otherwise relevo reads the file `relevo connect`
    saved, the one RELEVO_CONFIG names if the profile sets it."""
    env = dict(os.environ)
    for name in ("RELEVO_URL", "RELEVO_TOKEN", "RELEVO_CONFIG", "RELEVO_CERT"):
        env.pop(name, None)
    for name in ("RELEVO_CONFIG", "RELEVO_CERT"):
        value = _profile_value(name)
        if value:
            env[name] = value
    if settings.source == client.CONFIGURED_HERE and settings.configured:
        env["RELEVO_URL"], env["RELEVO_TOKEN"] = settings.url, settings.token
    return env


def restart_notice(loaded: Optional[str], on_disk: str, installed: str = "") -> str:
    """What the person is told when the plugin's folder holds `on_disk` and this gateway runs
    `loaded` (`installed`: what relevo printed, when this process's update installed it)."""
    text = (f"Relevo was updated on this machine: the Hermes plugin is now {on_disk}, and this gateway "
            f"still runs {loaded or 'an older one'}. Restart the Hermes gateway to use it (`hermes gateway "
            "restart`) when it suits you: it was not restarted, so the assistants keep working meanwhile.")
    return f"{text}\n\n{installed}" if installed else text


def stale(folder: Optional[Path] = None) -> Optional[str]:
    """The version in the plugin's folder when it is not the one this process loaded, else None."""
    on_disk = plugin_version(folder or FOLDER)
    return on_disk if on_disk is not None and LOADED is not None and on_disk != LOADED else None


async def tell_if_stale(tell: Tell, folder: Optional[Path] = None, installed: str = "") -> bool:
    """When the plugin's folder holds another version than this process runs, tells the person that
    the gateway must be restarted, once per version: again a few times (RETRY_DELAYS) while no home
    channel takes it. Returns whether it was told (now or before); False when there is nothing to
    tell, or nobody took it."""
    for delay in (*RETRY_DELAYS, None):
        on_disk = stale(folder)
        if on_disk is None:
            return False
        if on_disk in _told:
            return True
        _told.add(on_disk)
        logger.info("[relevo] the plugin's folder holds %s and the gateway runs %s: it takes a restart",
                    on_disk, LOADED)
        try:
            where = await tell(restart_notice(LOADED, on_disk, installed))
        except Exception as exc:
            logger.warning("[relevo] could not tell of the update: %s", exc)
            where = None
        if where:
            return True
        _told.discard(on_disk)
        if delay is None:
            return False
        await asyncio.sleep(delay)
    return False


def start(settings: client.Settings, tell: Tell, folder: Optional[Path] = None) -> bool:
    """Starts `relevo update --auto --adapter hermes --path <folder>` and returns at once: it never
    holds connect(). When relevo ends, what it installed is logged and, when the folder now holds
    another version than this process runs, `tell` (a coroutine run on this event loop) gets the
    restart notice (tell_if_stale); what failed is logged as a warning. A folder already newer than
    this process is told of at once, even when no update runs. Nothing happens when the
    update is turned off, there is no relevo to run, the folder is in a git checkout or an update
    from this process is running. Returns whether it started. Called from the event loop."""
    folder = folder or FOLDER
    loop = asyncio.get_running_loop()
    # Already updated by another client of this machine (a relevo mcp, pi), or a notice no home
    # channel took before: told now, whatever this run does.
    if stale(folder) is not None:
        task = loop.create_task(tell_if_stale(tell, folder), name="relevo-update-notice")
        _notices.add(task)  # the loop keeps only a weak reference
        task.add_done_callback(_notices.discard)
    if auto_update_off() or in_git_checkout(folder):
        return False
    relevo = find_relevo()
    if relevo is None or relevo in _no_updater or not _running.acquire(blocking=False):
        return False
    try:
        proc = subprocess.Popen(
            [relevo, "update", "--auto", "--adapter", "hermes", "--path", str(folder)],
            env=child_env(settings), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, close_fds=True,
            # Its own session: a gateway stopping (Ctrl-C included) does not cut an update short.
            start_new_session=True)
    except OSError as exc:
        _running.release()
        logger.warning("[relevo] could not run %s update: %s", relevo, exc)
        return False

    def wait() -> None:
        try:
            out, err = proc.communicate()
        finally:
            _running.release()
        failed = err.decode("utf-8", "replace").strip()
        if has_no_updater(proc.returncode, failed):
            _no_updater.add(relevo)
            logger.warning("[relevo] %s is older than 0.4.0 and cannot update itself nor this plugin: update it "
                           "by hand once, and it follows the server after that", relevo)
            return
        _report(proc.returncode, out.decode("utf-8", "replace").strip(), failed, tell, loop, folder)

    # A daemon thread: the gateway may exit while it waits, and the update goes on without it.
    threading.Thread(target=wait, name="relevo-update", daemon=True).start()
    return True


def _report(code: int, installed: str, failed: str, tell: Tell, loop: asyncio.AbstractEventLoop,
            folder: Path) -> None:
    if installed:
        for line in installed.splitlines():
            logger.info("[relevo] update: %s", line)
    # Only a new plugin in this folder takes a restart; a new relevo binary or pi extension does not.
    if stale(folder) is not None:
        try:
            asyncio.run_coroutine_threadsafe(tell_if_stale(tell, folder, installed), loop)
        except RuntimeError:
            # The gateway's loop has closed: its next start runs the new plugin anyway.
            pass
    if code != 0:
        logger.warning("[relevo] could not update itself (relevo update exited with %s): %s", code,
                       failed or "it said nothing")


def _home_channels(runner: Any, profile: Optional[str]):
    """(platform, home, transport) for each home channel of `profile` (None: the gateway's own)
    with a live transport, Relevo's own left out: its chats are teammates' threads."""
    served = getattr(runner, "_served_home_channel_transports", None)
    if callable(served):
        rows = ((p, platform, home, transport) for p, platform, _cfg, home, transport in served())
    else:
        rows = ((None, platform, home, transport) for platform, _cfg, home, transport
                in runner._home_channel_transports())
    for p, platform, home, transport in rows:
        if p == profile and getattr(platform, "value", platform) != "relevo":
            yield platform, home, transport


async def tell_home(runner: Any, profile: Optional[str], text: str) -> Optional[str]:
    """Sends `text` once, to the first home channel of `profile` that takes it, as the gateway sends
    its own notices there. Returns where it went ("telegram:123"), or None, logged, when no home
    channel took it."""
    try:
        for platform, home, transport in _home_channels(runner, profile) if runner is not None else ():
            name = getattr(platform, "value", str(platform))
            if await runner._send_home_channel_message(
                    platform, home, transport, text, "[relevo] could not tell %s:%s of the update: %s"):
                logger.info("[relevo] told %s:%s that the gateway must be restarted", name, home.chat_id)
                return f"{name}:{home.chat_id}"
    except Exception as exc:
        logger.warning("[relevo] could not tell a home channel of the update: %s", exc)
    logger.warning("[relevo] no Hermes home channel took the update notice: restart the gateway to use "
                   "the new plugin (`hermes gateway restart`)")
    return None
