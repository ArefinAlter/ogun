"""Secrets: values that exist only in memory, are read from the environment, and never print.

A configuration file never holds a secret. It names an environment variable instead, as
``{"env": "NAME"}``, and :func:`resolve` turns that into a :class:`Secret`.
"""

from __future__ import annotations

import os
import re
from typing import Any, Dict, List, Mapping, NamedTuple, Optional

_ENV_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,127}$")

#: Member names that hold a secret. Their value must be ``{"env": "NAME"}``.
SECRET_NAMES = frozenset(
    {
        "api_key",
        "token",
        "password",
        "secret",
        "webhook_url",
        "routing_key",
        "signing_secret",
        "key",
        "auth",
        "authorization",
        "header_value_env",
    }
)
_SECRET_SUFFIXES = ("_key", "_token", "_password", "_secret")

SECRET_HINT = 'a secret must be {"env": "NAME"}'


class ConfigError(Exception):
    """The configuration is invalid. Messages name keys and variables, never values."""


class EnvRef(NamedTuple):
    """An unresolved ``{"env": NAME}`` reference."""

    name: str


class Secret:
    """A secret value. ``repr`` and ``str`` print ``<secret>``; it cannot be serialised or copied."""

    __slots__ = ("_value",)

    def __init__(self, value: str) -> None:
        if not isinstance(value, str) or not value:
            raise ValueError("a secret is a non-empty string")
        self._value = value

    def reveal(self) -> str:
        """The value. Call it only where the value goes on the wire."""
        return self._value

    def __repr__(self) -> str:
        return "<secret>"

    def __str__(self) -> str:
        return "<secret>"

    def __format__(self, spec: str) -> str:
        return "<secret>"

    def __reduce__(self) -> Any:
        raise TypeError("a Secret cannot be serialised")

    def __reduce_ex__(self, protocol: Any) -> Any:
        raise TypeError("a Secret cannot be serialised")


def looks_like_secret(name: str) -> bool:
    """Whether a configuration member of this name must be an environment reference."""
    n = name.lower()
    return n in SECRET_NAMES or n.endswith(_SECRET_SUFFIXES)


def is_env_spec(value: Any) -> bool:
    """Whether ``value`` is exactly ``{"env": "NAME"}`` with a valid variable name."""
    return (
        isinstance(value, dict)
        and set(value.keys()) == {"env"}
        and isinstance(value["env"], str)
        and _ENV_NAME.match(value["env"]) is not None
    )


def _where(path: str) -> str:
    return f"{path}: " if path else ""


def parse_spec(spec: Any, path: str = "") -> EnvRef:
    """Checks the shape of an environment reference without reading the environment."""
    if not is_env_spec(spec):
        raise ConfigError(f"{_where(path)}{SECRET_HINT}")
    return EnvRef(spec["env"])


def resolve(spec: Any, environ: Optional[Mapping[str, str]] = None, path: str = "") -> Secret:
    """Reads the variable a reference names. A missing or empty variable is a ``ConfigError``
    that names the variable; no message ever contains a value."""
    ref = parse_spec(spec, path)
    env = os.environ if environ is None else environ
    raw = env.get(ref.name)
    if raw is None or not raw.strip():
        raise ConfigError(f"{_where(path)}environment variable {ref.name} is not set or is empty")
    value = raw.strip()
    if any(ord(c) < 0x20 or ord(c) > 0x7E for c in value):
        raise ConfigError(f"{_where(path)}environment variable {ref.name} has characters outside printable ASCII")
    return Secret(value)


def env_is_set(name: str, environ: Optional[Mapping[str, str]] = None) -> bool:
    """Whether a variable is set to a non-empty value. Never returns the value."""
    env = os.environ if environ is None else environ
    raw = env.get(name)
    return raw is not None and bool(raw.strip())


def env_names(value: Any) -> List[str]:
    """Every variable name referenced anywhere inside a configuration value, in order, unique."""
    found: List[str] = []

    def walk(v: Any) -> None:
        if is_env_spec(v):
            if v["env"] not in found:
                found.append(v["env"])
        elif isinstance(v, dict):
            for item in v.values():
                walk(item)
        elif isinstance(v, list):
            for item in v:
                walk(item)

    walk(value)
    return found


def reject_literal_secrets(cfg: Dict[str, Any], path: str) -> None:
    """Refuses any member whose name looks like a secret unless it is an environment reference.
    The message names the member and never prints what it holds."""
    for key, value in cfg.items():
        if looks_like_secret(str(key)) and not is_env_spec(value):
            raise ConfigError(f"{path}.{key}: {SECRET_HINT}")
