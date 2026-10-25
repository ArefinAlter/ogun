"""The JSON configuration file, version 1.

.. code-block:: json

    {"version": 1, "credential": "export", "ogun_home": null, "profile": null,
     "state_dir": "~/.ogun-export", "interval_s": 15, "snapshot_interval_s": 60,
     "sinks": [{"name": "dd", "type": "datadog", "site": "datadoghq.com",
                "api_key": {"env": "DD_API_KEY"}, "tags": ["env:prod"], "min_severity": "warning"}]}

Unknown keys are errors. A member whose name looks like a secret must be ``{"env": "NAME"}``; a
literal there is refused without printing it. Error messages name keys and variables, never values.
"""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional

from .secrets import ConfigError, env_names, reject_literal_secrets
from .sinks import REGISTRY, Sink
from .sinks.base import NAME_RE

__all__ = ["Config", "ConfigError", "build_sinks", "load_config", "parse_config", "required_env"]

_TOP_KEYS = ("version", "credential", "ogun_home", "profile", "state_dir", "interval_s", "snapshot_interval_s", "sinks")
_CREDENTIAL = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$")


@dataclass
class Config:
    """A validated configuration. Sinks stay as plain dicts until :func:`build_sinks`."""

    credential: str = "export"
    ogun_home: Optional[str] = None
    profile: Optional[str] = None
    state_dir: str = "~/.ogun-export"
    interval_s: float = 15
    snapshot_interval_s: float = 60
    sinks: List[Dict[str, Any]] = field(default_factory=list)

    @property
    def state_path(self) -> Path:
        """``state_dir`` with ``~`` expanded."""
        return Path(os.path.expanduser(self.state_dir))


def _optional_text(data: Dict[str, Any], key: str, default: Optional[str]) -> Optional[str]:
    value = data.get(key, default)
    if value is None:
        return None
    if not isinstance(value, str) or not value.strip() or len(value) > 1024 or "\x00" in value:
        raise ConfigError(f"{key}: must be a non-empty string or null")
    return value


def _number(data: Dict[str, Any], key: str, default: float, lo: float, hi: float) -> float:
    value = data.get(key, default)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ConfigError(f"{key}: must be a number")
    if value < lo or value > hi:
        raise ConfigError(f"{key}: must be between {lo:g} and {hi:g}")
    return value


def parse_config(data: Any) -> Config:
    """Validates a decoded configuration. Does not read the environment."""
    if not isinstance(data, dict):
        raise ConfigError("the configuration must be a JSON object")
    reject_literal_secrets(data, "config")
    for key in data:
        if key not in _TOP_KEYS:
            raise ConfigError(f"unknown key {str(key)[:40]!r}")
    if data.get("version") != 1 or isinstance(data.get("version"), bool):
        raise ConfigError("version: must be 1")
    credential = data.get("credential", "export")
    if not isinstance(credential, str) or not _CREDENTIAL.match(credential):
        raise ConfigError("credential: must be a credential name such as export")
    state_dir = _optional_text(data, "state_dir", "~/.ogun-export") or "~/.ogun-export"
    sinks = data.get("sinks")
    if not isinstance(sinks, list) or not sinks:
        raise ConfigError("sinks: must be a non-empty list")
    names = set()
    for i, raw in enumerate(sinks):
        path = f"sinks[{i}]"
        if not isinstance(raw, dict):
            raise ConfigError(f"{path}: must be an object")
        reject_literal_secrets(raw, path)
        name = raw.get("name")
        if not isinstance(name, str) or not NAME_RE.match(name):
            raise ConfigError(f"{path}.name: must match ^[a-z][a-z0-9_-]{{0,31}}$")
        if name in names:
            raise ConfigError(f"{path}.name: sink name {name!r} is used twice")
        names.add(name)
        kind = raw.get("type")
        cls = REGISTRY.get(kind) if isinstance(kind, str) else None
        if cls is None:
            raise ConfigError(f"{path}.type: must be one of {', '.join(REGISTRY)}")
        cls.validate(raw, path)
    return Config(
        credential=credential,
        ogun_home=_optional_text(data, "ogun_home", None),
        profile=_optional_text(data, "profile", None),
        state_dir=state_dir,
        interval_s=_number(data, "interval_s", 15, 1, 3600),
        snapshot_interval_s=_number(data, "snapshot_interval_s", 60, 1, 86400),
        sinks=[dict(s) for s in sinks],
    )


def load_config(path: Any) -> Config:
    """Reads and validates a configuration file."""
    try:
        text = Path(path).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as e:
        raise ConfigError(f"cannot read the configuration file ({type(e).__name__})") from None
    try:
        data = json.loads(text)
    except ValueError as e:
        line = getattr(e, "lineno", "?")
        col = getattr(e, "colno", "?")
        raise ConfigError(f"the configuration file is not valid JSON (line {line}, column {col})") from None
    return parse_config(data)


def required_env(config: Config) -> List[str]:
    """Every environment variable the configuration names, in order, unique."""
    return env_names(config.sinks)


def build_sinks(config: Config, environ: Optional[Mapping[str, str]] = None) -> List[Sink]:
    """Builds every sink, reading the environment variables it names."""
    sinks: List[Sink] = []
    for i, raw in enumerate(config.sinks):
        cls = REGISTRY[raw["type"]]
        sinks.append(cls.from_config(raw, environ, f"sinks[{i}]"))
    return sinks
