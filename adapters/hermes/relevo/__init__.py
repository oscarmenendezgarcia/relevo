"""Relevo plugin for Hermes: the ``relevo`` gateway platform and the ``relevo`` client tools.

Kept import-light: the adapter (and the gateway base classes it needs) load inside register().
"""

from __future__ import annotations

import dataclasses
import logging

logger = logging.getLogger(__name__)

__all__ = ["register"]


def check_requirements() -> bool:
    """Passive probe: httpx is a Hermes core dependency, so this is only a sanity check."""
    try:
        import httpx  # noqa: F401
    except ImportError:
        return False
    return True


def validate_config(config) -> bool:
    from .client import resolve_settings

    settings = resolve_settings(getattr(config, "extra", None) or {})
    return settings.configured and not settings.unpaired


def is_connected(config) -> bool:
    """Enabled from env or config.yaml only: a token saved by `relevo connect` for other agents does
    not by itself put this gateway on Relevo."""
    from gateway.platforms._shared import extra_or_secret

    extra = getattr(config, "extra", None) or {}
    return bool(str(extra_or_secret(extra, "url", "RELEVO_URL") or "").strip()
                and str(extra_or_secret(extra, "token", "RELEVO_TOKEN") or "").strip())


def _env_enablement() -> dict | None:
    """Auto-enable the platform when the profile's env names a server and a token. The token stays
    in the env: the adapter reads it from there, so it is never copied into the gateway config."""
    from gateway.platforms._shared import get_scoped_secret, seed_extra_from_env

    if not (str(get_scoped_secret("RELEVO_URL", "") or "").strip()
            and str(get_scoped_secret("RELEVO_TOKEN", "") or "").strip()):
        return None
    return seed_extra_from_env((("RELEVO_URL", "url", None), ("RELEVO_CONTEXT", "context", None)))


def register(ctx) -> None:
    try:
        from .tools import register_tools

        register_tools(ctx)
    except Exception:
        logger.warning("Relevo: failed to register client tools", exc_info=True)

    try:
        from .waiting import command

        # /relevo: what waits on the person, numbered; its numbers go to the agent (docs/agents.md).
        ctx.register_command("relevo", command(ctx),
                             description="Relevo · what is waiting on you, and answer it from here",
                             args_hint="[N <your answer> | N | seen N… | @name <note>]")
    except Exception:
        logger.warning("Relevo: failed to register the /relevo command", exc_info=True)

    from .client import PLATFORM_HINT

    def _factory(cfg):
        from .adapter import RelevoAdapter

        return RelevoAdapter(cfg)

    kwargs = dict(
        name="relevo", label="Relevo", adapter_factory=_factory,
        check_fn=check_requirements, validate_config=validate_config, is_connected=is_connected,
        required_env=["RELEVO_URL", "RELEVO_TOKEN"],
        install_hint="No extra packages needed (httpx ships with Hermes)",
        env_enablement_fn=_env_enablement,
        allowed_users_env="RELEVO_ALLOWED_USERS", allow_all_env="RELEVO_ALLOW_ALL_USERS",
        # A note holds at most 64 KiB of UTF-8; 16,000 characters always fit.
        max_message_length=16000,
        allow_update_command=False,
        platform_hint=PLATFORM_HINT,
        emoji="\U0001f4e8",
    )
    # Feature-detect fields that older cores reject.
    try:
        from gateway.platform_registry import PlatformEntry

        known = {f.name for f in dataclasses.fields(PlatformEntry)}
    except Exception:
        known = set()
    for key, value in (
        # No tool progress, streaming or interim messages by default. The owner's global display
        # keys beat a tier, so RelevoAdapter.send() is what keeps everything but the final out.
        ("display_tier", "minimal"),
        # With RELEVO_ALLOWED_USERS set, unknown senders are ignored instead of sent a pairing code.
        ("allowlist_suppresses_pairing", True),
        # Profile clones keep RELEVO_* for the tools when this profile does not run the adapter.
        ("shared_env_prefixes", ("RELEVO_",)),
    ):
        if key in known:
            kwargs[key] = value
    ctx.register_platform(**kwargs)
