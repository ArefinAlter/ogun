"""The local IPC transport (O-15 sections 2 and 3): a Unix domain socket or a Windows named pipe,
carrying newline-delimited JSON. Requests are at most 1 MiB, responses at most 16 MiB."""

from __future__ import annotations

import io
import socket
import time
from typing import Callable, Optional

from .errors import OgunError, OgunUnavailable
from .paths import Endpoint

MAX_REQUEST_BYTES = 1024 * 1024
MAX_RESPONSE_BYTES = 16 * 1024 * 1024
_CHUNK = 64 * 1024
_ERROR_PIPE_BUSY = 231


class Transport:
    """A connected byte stream with line framing."""

    def __init__(self, read: Callable[[int], bytes], write: Callable[[bytes], None], close: Callable[[], None]) -> None:
        self._read = read
        self._write = write
        self._close = close
        self._buffer = bytearray()
        self.closed = False

    def send_line(self, data: bytes) -> None:
        """Writes one message and its newline."""
        if len(data) > MAX_REQUEST_BYTES:
            raise OgunError("api.payload_too_large", "request longer than 1 MiB")
        try:
            self._write(data + b"\n")
        except OSError as e:
            raise OgunUnavailable("api.unavailable", f"write failed: {type(e).__name__}") from None

    def read_line(self, limit: int = MAX_RESPONSE_BYTES) -> Optional[bytes]:
        """Reads one message; ``None`` at a clean end of stream."""
        while True:
            nl = self._buffer.find(b"\n")
            if nl >= 0:
                line = bytes(self._buffer[:nl])
                del self._buffer[: nl + 1]
                return line
            if len(self._buffer) > limit:
                raise OgunError("api.payload_too_large", "response longer than the line limit")
            try:
                chunk = self._read(_CHUNK)
            except socket.timeout:
                raise OgunUnavailable("api.unavailable", "ogund did not answer in time") from None
            except OSError as e:
                raise OgunUnavailable("api.unavailable", f"read failed: {type(e).__name__}") from None
            if not chunk:
                if self._buffer:
                    raise OgunUnavailable("api.unavailable", "connection closed mid-message")
                return None
            self._buffer.extend(chunk)

    def close(self) -> None:
        """Closes the stream."""
        if not self.closed:
            self.closed = True
            try:
                self._close()
            except OSError:
                pass


def _connect_unix(path: str, timeout: Optional[float]) -> Transport:
    if not hasattr(socket, "AF_UNIX"):
        raise OgunUnavailable("api.unavailable", "Unix sockets are not available on this platform")
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(timeout)
    try:
        sock.connect(path)
    except OSError as e:
        sock.close()
        raise OgunUnavailable("api.unavailable", f"ogund is not running ({type(e).__name__})") from None
    return Transport(sock.recv, sock.sendall, sock.close)


def _connect_pipe(name: str, timeout: Optional[float]) -> Transport:
    deadline = time.monotonic() + (timeout if timeout is not None else 1.0)
    while True:
        try:
            f = io.open(name, "r+b", buffering=0)
            break
        except OSError as e:
            if getattr(e, "winerror", None) == _ERROR_PIPE_BUSY and time.monotonic() < deadline:
                time.sleep(0.05)
                continue
            raise OgunUnavailable("api.unavailable", f"ogund is not running ({type(e).__name__})") from None

    def write(data: bytes) -> None:
        view = memoryview(data)
        while view:
            n = f.write(view)
            view = view[n or 0:]
        f.flush()

    # Reads on a pipe block; the per-call timeout applies only to Unix sockets in this draft.
    return Transport(f.read, write, f.close)


def connect(endpoint: Endpoint, timeout: Optional[float] = 30.0) -> Transport:
    """Connects to the daemon's endpoint."""
    if endpoint.kind == "unix":
        return _connect_unix(endpoint.address, timeout)
    if endpoint.kind == "pipe":
        return _connect_pipe(endpoint.address, timeout)
    raise OgunUnavailable("api.unavailable", f"unknown endpoint kind {endpoint.kind}")
