"""Where Ogun keeps its state and where ``ogund`` listens (O-15 section 2).

This mirrors ``ogun_api::endpoint::OgunPaths`` exactly, including the BLAKE3-derived fallback
socket path and the Windows pipe name, so a Python program finds the same daemon as the ``ogun``
CLI. Paths are joined as strings, the way Rust's ``PathBuf::join`` does, because the endpoint
name is derived from the text of the state directory.
"""

from __future__ import annotations

import os
import sys
from dataclasses import dataclass
from typing import Mapping, Optional

import blake3

MAX_SOCKET_PATH_BYTES = 100


@dataclass(frozen=True)
class Endpoint:
    """Where the daemon listens."""

    #: ``"unix"`` for a Unix domain socket, ``"pipe"`` for a Windows named pipe.
    kind: str
    #: The socket path, or the full pipe name ``\\\\.\\pipe\\...``.
    address: str

    def __str__(self) -> str:
        return self.address


def _join(base: str, *parts: str, windows: bool) -> str:
    sep = "\\" if windows else "/"
    out = base
    for p in parts:
        if out.endswith("/") or (windows and out.endswith("\\")):
            out = out + p
        else:
            out = out + sep + p
    return out


@dataclass(frozen=True)
class OgunPaths:
    """Every path Ogun uses for one user and optional profile."""

    state_dir: str
    runtime_dir: str
    profile: Optional[str] = None
    windows: bool = os.name == "nt"

    @classmethod
    def rooted(cls, base: str, profile: Optional[str] = None, *, windows: Optional[bool] = None) -> "OgunPaths":
        """Paths under one directory, as for ``OGUN_HOME``."""
        win = (os.name == "nt") if windows is None else windows
        base = os.fspath(base)
        state = _join(base, "profiles", profile, windows=win) if profile else base
        return cls(state_dir=state, runtime_dir=_join(state, "run", windows=win), profile=profile, windows=win)

    @classmethod
    def discover(
        cls,
        profile: Optional[str] = None,
        *,
        env: Optional[Mapping[str, str]] = None,
        platform: Optional[str] = None,
    ) -> "OgunPaths":
        """Discovers the paths for the current user, as ``ogun`` and ``ogund`` do.

        ``$OGUN_HOME`` wins. Otherwise: Linux and other Unix ``$XDG_STATE_HOME/ogun`` (default
        ``~/.local/state/ogun``) with the socket under ``$XDG_RUNTIME_DIR``; macOS
        ``~/Library/Application Support/Ogun``; Windows ``%LOCALAPPDATA%\\Ogun``.
        """
        env = os.environ if env is None else env
        plat = sys.platform if platform is None else platform
        windows = plat.startswith("win")
        home_override = env.get("OGUN_HOME")
        if home_override:
            return cls.rooted(home_override, profile, windows=windows)
        home = env.get("HOME") or env.get("USERPROFILE") or os.path.expanduser("~")
        runtime: Optional[str] = None
        if windows:
            base = env.get("LOCALAPPDATA") or _join(home, "AppData", "Local", windows=True)
            name = "Ogun"
        elif plat == "darwin":
            base = _join(home, "Library", "Application Support", windows=False)
            name = "Ogun"
        else:
            xdg = env.get("XDG_STATE_HOME", "")
            base = xdg if os.path.isabs(xdg) else _join(home, ".local", "state", windows=False)
            xdg_run = env.get("XDG_RUNTIME_DIR", "")
            runtime = xdg_run if os.path.isabs(xdg_run) else None
            name = "ogun"
        state = _join(base, name, windows=windows)
        if profile:
            state = _join(state, "profiles", profile, windows=windows)
        return cls(
            state_dir=state,
            runtime_dir=runtime if runtime is not None else _join(state, "run", windows=windows),
            profile=profile,
            windows=windows,
        )

    def _digest(self) -> str:
        identity = f"{self.state_dir}|{self.profile or ''}"
        return blake3.blake3(identity.encode("utf-8")).hexdigest()

    def endpoint(self, *, temp_dir: Optional[str] = None) -> Endpoint:
        """The daemon endpoint for these paths."""
        digest = self._digest()
        if self.windows:
            return Endpoint("pipe", "\\\\.\\pipe\\ogund-" + digest[:16])
        sock = f"ogund-{self.profile}.sock" if self.profile else "ogund.sock"
        preferred = _join(self.runtime_dir, "ogun", sock, windows=False)
        if len(preferred.encode("utf-8")) <= MAX_SOCKET_PATH_BYTES:
            return Endpoint("unix", preferred)
        tmp = temp_dir if temp_dir is not None else (os.environ.get("TMPDIR") or "/tmp")
        return Endpoint("unix", _join(tmp, f"ogun-{digest[:12]}", "ogund.sock", windows=False))

    def ipc_secret_file(self) -> str:
        """The per-user IPC secret (``0600``)."""
        return _join(self.state_dir, "ipc.secret", windows=self.windows)

    def credentials_dir(self) -> str:
        """The directory of credential token files."""
        return _join(self.state_dir, "credentials", windows=self.windows)

    def credential_file(self, name: str) -> str:
        """One credential token file, ``credentials/<name>.token``."""
        return _join(self.credentials_dir(), f"{name}.token", windows=self.windows)
