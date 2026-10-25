"""``ogun-export``: ``run``, ``check`` and ``sinks``.

Exit codes: 0 success; 1 a runtime failure (the daemon cannot be reached, the credential is
refused, a delivery is still pending after ``run --once``); 2 a usage or configuration error.
Only this module imports ``ogun-client``, and only when it connects.
"""

from __future__ import annotations

import argparse
import logging
import os
import signal
import sys
import threading
import time
from typing import Any, Callable, Dict, List, Mapping, Optional, Sequence, TextIO

from . import __version__
from .config import Config, ConfigError, build_sinks, load_config, required_env
from .outbox import describe
from .runner import Exporter, ReconnectingClient
from .secrets import env_is_set, env_names
from .sinks import REGISTRY

ClientFactory = Callable[[Config], Any]


class CliError(Exception):
    """A runtime failure with a message that is safe to print."""


def _safe(exc: BaseException) -> str:
    """Type and code of an error, plus the client's own message when it has a code (the client's
    messages never contain secrets)."""
    code = getattr(exc, "code", None)
    message = getattr(exc, "message", None)
    if isinstance(code, str) and isinstance(message, str):
        return f"{describe(exc)}: {message}"
    return describe(exc)


def default_client_factory(cfg: Config) -> Any:
    """Connects with ``ogun_client.Client`` using the configured ``agent`` credential."""
    try:
        from ogun_client import Client
    except ImportError:
        raise CliError("ogun-client is not installed; install it with: pip install ogun-client") from None
    client = Client.connect(
        cfg.credential,
        ogun_home=cfg.ogun_home,
        profile=cfg.profile,
        client_name="ogun-export",
        client_version=__version__,
    )
    kind = getattr(client, "kind", None)
    if kind != "agent":
        client.close()
        raise CliError("the credential is not of kind agent; the exporter refuses to run with it")
    return client


def _table(rows: Sequence[Sequence[str]], out: TextIO) -> None:
    widths = [max(len(r[i]) for r in rows) for i in range(len(rows[0]))]
    for r in rows:
        print("  ".join(cell.ljust(widths[i]) for i, cell in enumerate(r)).rstrip(), file=out)


def _parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="ogun-export",
        description="Sends Ogun run, partition and approval information to observability, lineage and chat systems. "
        "Read-only: it uses an agent credential and can never approve anything.",
    )
    p.add_argument("-v", "--verbose", action="store_true", help="log debug messages")
    sub = p.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run", help="follow the daemon and deliver events")
    run.add_argument("--config", required=True, metavar="FILE")
    run.add_argument("--once", action="store_true", help="one pass, then exit (1 if something is still undelivered)")
    check = sub.add_parser("check", help="validate the configuration and list what it will contact")
    check.add_argument("--config", required=True, metavar="FILE")
    check.add_argument("--offline", action="store_true", help="do not connect to the daemon")
    sub.add_parser("sinks", help="list the sink catalogue")
    return p


def _cmd_sinks(out: TextIO) -> int:
    for cls in REGISTRY.values():
        c = cls.catalogue()
        print(c["type"], file=out)
        print(f"  {c['description']}", file=out)
        print(f"  capabilities: {', '.join(c['capabilities'])}", file=out)
        print(f"  egress: {c['egress']}", file=out)
        keys = ", ".join(f"{k}{'*' if req else ''}{' (secret)' if k in c['secrets'] else ''}" for k, req, _ in c["keys"])
        print(f"  keys: {keys}   (* required)", file=out)
    return 0


def _severity_cell(cls: Any, raw: Mapping[str, Any]) -> str:
    if "events" not in cls.capabilities:
        return "-"
    if not cls.MIN_SEVERITY_OK:
        return "error, attention"
    return str(raw.get("min_severity") or "warning")


def _cmd_check(args: argparse.Namespace, env: Mapping[str, str], factory: ClientFactory, out: TextIO, err: TextIO) -> int:
    try:
        cfg = load_config(args.config)
    except ConfigError as e:
        print(f"configuration error: {e}", file=err)
        return 2
    names = required_env(cfg)
    missing = [n for n in names if not env_is_set(n, env)]
    if not missing:
        try:
            build_sinks(cfg, env)
        except ConfigError as e:
            print(f"configuration error: {e}", file=err)
            return 2
    print(f"configuration: valid, {len(cfg.sinks)} sinks, credential {cfg.credential}, state in {cfg.state_dir}", file=out)
    rows = [["sink", "type", "capabilities", "min severity", "will contact"]]
    for raw in cfg.sinks:
        cls = REGISTRY[raw["type"]]
        rows.append(
            [
                raw["name"],
                cls.type,
                ",".join(sorted(cls.capabilities)),
                _severity_cell(cls, raw),
                ", ".join(cls.egress_hosts(raw, env)) or "-",
            ]
        )
    _table(rows, out)
    if names:
        print("", file=out)
        env_rows = [["environment variable", "state", "used by"]]
        for n in names:
            users = [s["name"] for s in cfg.sinks if n in env_names(s)]
            env_rows.append([n, "set" if n not in missing else "NOT SET", ", ".join(users)])
        _table(env_rows, out)
    if missing:
        print(f"{len(missing)} environment variable(s) are not set", file=err)
        return 2
    if args.offline:
        return 0
    try:
        client = factory(cfg)
        try:
            status = client.status()
        finally:
            closer = getattr(client, "close", None)
            if callable(closer):
                closer()
    except Exception as e:  # noqa: BLE001 - reported without values
        reason = str(e) if isinstance(e, CliError) else _safe(e)
        print(f"daemon: cannot reach ogund ({reason})", file=err)
        return 1
    version = status.get("daemon_version") if isinstance(status, dict) else None
    print("", file=out)
    print(f"daemon: ogund {version or 'unknown version'} reachable with an agent credential", file=out)
    return 0


def _install_signals(stop: threading.Event) -> None:
    def handler(signum: int, frame: Any) -> None:
        stop.set()

    for name in ("SIGINT", "SIGTERM", "SIGBREAK"):
        sig = getattr(signal, name, None)
        if sig is not None:
            try:
                signal.signal(sig, handler)
            except (ValueError, OSError):
                pass


def _cmd_run(
    args: argparse.Namespace,
    env: Mapping[str, str],
    factory: ClientFactory,
    out: TextIO,
    err: TextIO,
    sleep: Callable[[float], None],
) -> int:
    try:
        cfg = load_config(args.config)
        sinks = build_sinks(cfg, env)
    except ConfigError as e:
        print(f"configuration error: {e}", file=err)
        return 2
    first: Any = None
    try:
        first = factory(cfg)
    except CliError as e:
        print(f"cannot start: {e}", file=err)
        return 1
    except Exception as e:  # noqa: BLE001 - the daemon may simply not be up yet
        if args.once:
            print(f"cannot reach ogund ({_safe(e)})", file=err)
            return 1
        logging.getLogger("ogun_export").warning("ogund is not reachable yet (%s); retrying", describe(e))
    client = ReconnectingClient(lambda: factory(cfg), initial=first)
    exporter = Exporter(
        client, sinks, cfg.state_path, interval_s=cfg.interval_s, snapshot_interval_s=cfg.snapshot_interval_s, sleep=sleep
    )
    try:
        if args.once:
            try:
                summary = exporter.run_once()
            except Exception as e:  # noqa: BLE001
                print(f"exporter failed: {_safe(e)}", file=err)
                return 1
            print(
                f"events: {summary['events_seen']} seen, {summary['events_exported']} exported; "
                f"snapshot: {'sent' if summary['snapshot'] else 'not due or not wanted'}",
                file=out,
            )
            for name, state in exporter.status()["sinks"].items():
                print(f"sink {name}: {state['pending']} pending, {state['dropped']} dropped", file=out)
            failed = bool(summary["daemon_error"]) or bool(summary["sink_errors"]) or any(summary["pending"].values())
            if summary["daemon_error"]:
                print(f"daemon: {summary['daemon_error']}", file=err)
            for name, message in summary["sink_errors"].items():
                print(f"sink {name}: {message}", file=err)
            return 1 if failed else 0
        stop = threading.Event()
        _install_signals(stop)
        logging.getLogger("ogun_export").info("ogun-export %s started with %d sinks", __version__, len(sinks))
        exporter.run_forever(stop)
        return 0
    finally:
        client.close()


def main(
    argv: Optional[Sequence[str]] = None,
    *,
    client_factory: Optional[ClientFactory] = None,
    environ: Optional[Mapping[str, str]] = None,
    stdout: Optional[TextIO] = None,
    stderr: Optional[TextIO] = None,
    setup_logging: bool = True,
    sleep: Callable[[float], None] = time.sleep,
) -> int:
    """The command line. ``client_factory``, ``environ`` and ``sleep`` exist for tests."""
    out = stdout if stdout is not None else sys.stdout
    err = stderr if stderr is not None else sys.stderr
    env: Mapping[str, str] = os.environ if environ is None else environ
    try:
        args = _parser().parse_args(argv)
    except SystemExit as e:
        return e.code if isinstance(e.code, int) else 2
    if setup_logging:
        logging.basicConfig(
            level=logging.DEBUG if args.verbose else logging.INFO,
            format="%(asctime)s %(levelname)s %(name)s %(message)s",
            stream=err,
        )
    factory = client_factory or default_client_factory
    if args.command == "sinks":
        return _cmd_sinks(out)
    if args.command == "check":
        return _cmd_check(args, env, factory, out, err)
    return _cmd_run(args, env, factory, out, err, sleep)


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
