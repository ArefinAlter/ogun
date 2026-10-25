"""The sink base class and the small schema each sink uses to validate its own configuration.

A sink declares which of ``events``, ``metrics`` and ``lineage`` it supports. The runner calls only
those. Configuration is validated against :class:`Field` declarations: unknown keys are errors,
and a member that holds a secret must be ``{"env": "NAME"}``.
"""

from __future__ import annotations

import json
import logging
import re
import time
import urllib.parse
from dataclasses import dataclass
from typing import Any, Callable, ClassVar, Dict, FrozenSet, List, Mapping, Optional, Sequence, Tuple

from .. import http
from ..http import DeliveryError
from ..model import MIN_SEVERITIES, SEVERITY_RANK, ExportEvent, Snapshot
from ..secrets import ConfigError, Secret, parse_spec, reject_literal_secrets, resolve

NAME_RE = re.compile(r"^[a-z][a-z0-9_-]{0,31}$")
_HEADER_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9-]{0,63}$")


@dataclass(frozen=True)
class Field:
    """One configuration member of a sink.

    ``kind`` is one of ``str``, ``enum``, ``bool``, ``num``, ``str_list``, ``secret`` (an
    environment reference), ``url`` (a plain URL without credentials), ``secret_url`` (an
    environment reference whose value is a URL) and ``env_map`` (header name to environment
    reference).
    """

    kind: str
    required: bool = False
    default: Any = None
    choices: Tuple[str, ...] = ()
    pattern: Optional[str] = None
    lo: Optional[float] = None
    hi: Optional[float] = None
    max_len: int = 256
    help: str = ""


def trunc(text: Optional[str], limit: int) -> str:
    """``text`` cut to ``limit`` characters."""
    s = text or ""
    return s if len(s) <= limit else s[: max(limit - 3, 0)] + "..."


def trunc_bytes(text: str, limit: int) -> str:
    """``text`` cut so that its UTF-8 form has at most ``limit`` bytes."""
    raw = text.encode("utf-8")
    if len(raw) <= limit:
        return text
    return raw[:limit].decode("utf-8", errors="ignore")


def dumps(obj: Any) -> bytes:
    """Compact JSON bytes."""
    return json.dumps(obj, separators=(",", ":"), allow_nan=False).encode("utf-8")


def host_of(url: Any) -> Optional[str]:
    """``host`` or ``host:port`` of a URL, or ``None``."""
    if not isinstance(url, str):
        return None
    try:
        parts = urllib.parse.urlsplit(url)
        host = parts.hostname
        port = parts.port
    except ValueError:
        return None
    if not host:
        return None
    host = f"[{host}]" if ":" in host else host
    return f"{host}:{port}" if port else host


def secret_url_host(cfg: Mapping[str, Any], key: str, environ: Optional[Mapping[str, str]], fallback: str) -> str:
    """The host of a URL held in an environment variable, or ``fallback`` when it is not set."""
    try:
        value = resolve(cfg.get(key), environ, key).reveal()
    except ConfigError:
        return fallback
    return host_of(value) or fallback


# ---- validation ---------------------------------------------------------------------------


def _text(value: Any, where: str, f: Field) -> str:
    if not isinstance(value, str):
        raise ConfigError(f"{where}: must be a string")
    s = value.strip()
    if not s:
        raise ConfigError(f"{where}: must not be empty")
    if len(s) > f.max_len:
        raise ConfigError(f"{where}: is longer than {f.max_len} characters")
    if any(ord(c) < 0x20 or ord(c) == 0x7F for c in s):
        raise ConfigError(f"{where}: contains control characters")
    if f.pattern and not re.fullmatch(f.pattern, s):
        raise ConfigError(f"{where}: does not match the expected format")
    return s


def _default(f: Field) -> Any:
    if f.kind == "str_list":
        return list(f.default or [])
    if f.kind == "env_map":
        return {}
    return f.default


def _parse_value(f: Field, value: Any, where: str, environ: Optional[Mapping[str, str]], resolve_env: bool, insecure: bool) -> Any:
    kind = f.kind
    if kind == "str":
        return _text(value, where, f)
    if kind == "enum":
        if value not in f.choices:
            raise ConfigError(f"{where}: must be one of {', '.join(f.choices)}")
        return value
    if kind == "bool":
        if not isinstance(value, bool):
            raise ConfigError(f"{where}: must be true or false")
        return value
    if kind == "num":
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ConfigError(f"{where}: must be a number")
        if (f.lo is not None and value < f.lo) or (f.hi is not None and value > f.hi):
            raise ConfigError(f"{where}: must be between {f.lo} and {f.hi}")
        return value
    if kind == "str_list":
        if not isinstance(value, list) or len(value) > 50:
            raise ConfigError(f"{where}: must be a list of at most 50 strings")
        item = Field("str", max_len=200)
        return [_text(v, f"{where}[{i}]", item) for i, v in enumerate(value)]
    if kind == "secret":
        return resolve(value, environ, where) if resolve_env else parse_spec(value, where)
    if kind == "secret_url":
        ref = resolve(value, environ, where) if resolve_env else parse_spec(value, where)
        if isinstance(ref, Secret):
            problem = http.url_problem(ref.reveal(), insecure)
            if problem:
                raise ConfigError(f"{where}: the value of the variable is not usable: {problem}")
        return ref
    if kind == "url":
        s = _text(value, where, f)
        try:
            parts = urllib.parse.urlsplit(s)
            parts.port  # noqa: B018 - validates the port
        except ValueError:
            raise ConfigError(f"{where}: is not a valid URL") from None
        if "@" in parts.netloc:
            raise ConfigError(f"{where}: must not contain credentials; use the secret options")
        if parts.query or parts.fragment or "?" in s or "#" in s:
            raise ConfigError(f"{where}: must not contain a query string or a fragment")
        problem = http.url_problem(s, insecure)
        if problem:
            raise ConfigError(f"{where}: {problem}")
        return s.rstrip("/")
    if kind == "env_map":
        if not isinstance(value, dict) or len(value) > 20:
            raise ConfigError(f'{where}: must be an object of header name to {{"env": "NAME"}}, at most 20')
        out: Dict[str, Any] = {}
        for header, spec in value.items():
            if not isinstance(header, str) or not _HEADER_NAME.match(header):
                raise ConfigError(f"{where}: {str(header)[:40]!r} is not a valid header name")
            sub = f"{where}.{header}"
            out[header] = resolve(spec, environ, sub) if resolve_env else parse_spec(spec, sub)
        return out
    raise ConfigError(f"{where}: unsupported field kind")  # pragma: no cover - a bug in a sink declaration


def parse_fields(
    fields: Mapping[str, Field],
    cfg: Any,
    path: str,
    environ: Optional[Mapping[str, str]],
    resolve_env: bool,
) -> Dict[str, Any]:
    """Validates ``cfg`` against ``fields``. With ``resolve_env`` false, secrets stay
    :class:`EnvRef` and the environment is not read."""
    if not isinstance(cfg, dict):
        raise ConfigError(f"{path}: must be an object")
    reject_literal_secrets(cfg, path)
    for key in cfg:
        if key not in fields:
            raise ConfigError(f"{path}: unknown key {str(key)[:40]!r}")
    insecure = cfg.get("allow_insecure_http") is True
    out: Dict[str, Any] = {}
    for key, f in fields.items():
        where = f"{path}.{key}"
        if cfg.get(key) is None:
            if f.required:
                raise ConfigError(f"{where}: is required")
            out[key] = _default(f)
            continue
        out[key] = _parse_value(f, cfg[key], where, environ, resolve_env, insecure)
    return out


# ---- the base class -----------------------------------------------------------------------


class Sink:
    """Sends events, snapshots and lineage to one external system."""

    type: ClassVar[str] = ""
    description: ClassVar[str] = ""
    #: Host names this sink contacts, for documentation (``check`` computes the real ones).
    egress_doc: ClassVar[str] = ""
    capabilities: ClassVar[FrozenSet[str]] = frozenset()
    FIELDS: ClassVar[Dict[str, Field]] = {}
    #: Whether the sink takes a configured URL (and so ``allow_insecure_http``).
    HAS_URL: ClassVar[bool] = False
    #: Whether ``min_severity`` applies. Event sinks only.
    MIN_SEVERITY_OK: ClassVar[bool] = True
    #: Items per request for sinks that batch.
    CHUNK: ClassVar[int] = 200

    def __init__(
        self,
        *,
        name: str,
        min_severity: str = "warning",
        timeout_s: float = 10.0,
        allow_insecure_http: bool = False,
    ) -> None:
        self.name = name
        self.min_severity = min_severity
        self.timeout_s = float(timeout_s)
        self.allow_insecure_http = bool(allow_insecure_http)
        #: Both are injected by the runner so that tests never wait and times are deterministic.
        self.sleep: Callable[[float], None] = time.sleep
        self.clock: Callable[[], float] = time.time
        self.log = logging.getLogger(f"ogun_export.sink.{name}")

    def __repr__(self) -> str:
        return f"<{type(self).__name__} name={self.name!r}>"

    # ---- configuration ----

    @classmethod
    def all_fields(cls) -> Dict[str, Field]:
        """Common members plus this sink's own."""
        fields: Dict[str, Field] = {
            "name": Field("str", required=True, pattern=NAME_RE.pattern, max_len=32),
            "type": Field("str", required=True),
        }
        if "events" in cls.capabilities and cls.MIN_SEVERITY_OK:
            fields["min_severity"] = Field("enum", default="warning", choices=MIN_SEVERITIES)
        fields["timeout_s"] = Field("num", default=10.0, lo=1, hi=120)
        if cls.HAS_URL:
            fields["allow_insecure_http"] = Field("bool", default=False)
        fields.update(cls.FIELDS)
        return fields

    @classmethod
    def cross_check(cls, params: Dict[str, Any], path: str) -> None:
        """Checks that depend on more than one member. Raise :class:`ConfigError`."""

    @classmethod
    def validate(cls, cfg: Any, path: str = "sink") -> Dict[str, Any]:
        """Structure check without reading the environment."""
        params = parse_fields(cls.all_fields(), cfg, path, None, False)
        cls.cross_check(params, path)
        return params

    @classmethod
    def from_config(cls, cfg: Any, environ: Optional[Mapping[str, str]] = None, path: str = "sink") -> "Sink":
        """Validates ``cfg``, reads the environment variables it names, and builds the sink."""
        params = parse_fields(cls.all_fields(), cfg, path, environ, True)
        cls.cross_check(params, path)
        params.pop("type", None)
        return cls(**params)

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        """Host names (with a port when one is configured) this sink will contact."""
        return []

    @classmethod
    def catalogue(cls) -> Dict[str, Any]:
        """A description for ``ogun-export sinks``."""
        keys = []
        secrets = []
        for key, f in cls.all_fields().items():
            if key in ("type",):
                continue
            keys.append((key, f.required, f.kind))
            if f.kind in ("secret", "secret_url", "env_map"):
                secrets.append(key)
        return {
            "type": cls.type,
            "description": cls.description,
            "capabilities": sorted(cls.capabilities),
            "keys": keys,
            "secrets": secrets,
            "egress": cls.egress_doc,
        }

    # ---- what the runner calls ----

    def accepts(self, event: ExportEvent) -> bool:
        """Whether this sink wants the event: severity at or above ``min_severity``, always
        ``attention``, and always ``run_finished``."""
        if "events" not in self.capabilities:
            return False
        if event.kind == "run_finished" or event.severity == "attention":
            return True
        return SEVERITY_RANK.get(event.severity, 0) >= SEVERITY_RANK[self.min_severity]

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        """Delivers a batch of events. Raises :class:`DeliveryError`."""
        raise NotImplementedError(f"sink {self.name} does not send events")

    def send_snapshot(self, snapshot: Snapshot) -> None:
        """Delivers daemon counters. Raises :class:`DeliveryError`."""
        raise NotImplementedError(f"sink {self.name} does not send metrics")

    def send_lineage(self, items: Sequence[Dict[str, Any]]) -> None:
        """Delivers OpenLineage run events. Raises :class:`DeliveryError`."""
        raise NotImplementedError(f"sink {self.name} does not send lineage")

    # ---- helpers for subclasses ----

    def _post(
        self,
        url: str,
        body: bytes,
        headers: Optional[Dict[str, str]] = None,
        *,
        method: str = "POST",
        content_type: str = "application/json",
        secret_url: bool = False,
    ) -> bytes:
        merged = {"Content-Type": content_type}
        merged.update(headers or {})
        return http.post(
            url,
            body,
            merged,
            self.timeout_s,
            method,
            sink=self.name,
            sleep=self.sleep,
            allow_insecure_http=self.allow_insecure_http,
            secret_url=secret_url,
        )

    def _post_json(self, url: str, obj: Any, headers: Optional[Dict[str, str]] = None, **kw: Any) -> bytes:
        return self._post(url, dumps(obj), headers, **kw)

    def _fail(self, url: str, reason: str, *, status: Optional[int] = None, secret_url: bool = False) -> DeliveryError:
        """A delivery error for a response the server sent with a success status but a failure body."""
        shown = http.redact_url(url, keep_path=not secret_url)
        return DeliveryError(f'delivery to sink "{self.name}" failed: {reason} ({shown})', status=status, sink=self.name)

    @staticmethod
    def _json(raw: bytes) -> Any:
        """Parses a response body; ``None`` when it is not JSON. The body is never logged."""
        try:
            return json.loads(raw.decode("utf-8")) if raw else None
        except (ValueError, UnicodeDecodeError):
            return None

    @staticmethod
    def _each(items: Sequence[Any], fn: Callable[[Any], None]) -> None:
        """Calls ``fn`` per item; a failure records how many items were delivered before it."""
        for i, item in enumerate(items):
            try:
                fn(item)
            except DeliveryError as e:
                e.delivered = i
                raise

    def _each_chunk(self, items: Sequence[Any], fn: Callable[[Sequence[Any]], None]) -> None:
        """Calls ``fn`` per chunk of ``CHUNK`` items; a failure records how many items were delivered."""
        for start in range(0, len(items), self.CHUNK):
            try:
                fn(items[start : start + self.CHUNK])
            except DeliveryError as e:
                e.delivered = start
                raise
