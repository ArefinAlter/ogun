"""Credentials and the mutual handshake (O-15 section 4.1), byte for byte as ``ogun_api::auth``.

::

    client                                              ogund
      daemon.hello {protocol, client, client_nonce}  ->
                   <-  {protocol, daemon_version, profile?, server_nonce, server_proof}
      check server_proof = BLAKE3-keyed(ipc_secret, "ogun-server-v1" || client_nonce)
      daemon.authenticate {credential_id, proof}     ->
                   <-  {kind, name, interactive, methods}

``auth_key = BLAKE3-derive-key("ogun 2026-10 client auth key v1", token)`` and
``proof = BLAKE3-keyed(auth_key, "ogun-client-v1" || credential_id || server_nonce || client_nonce)``,
with the nonces as their 64-character hex text. The token never crosses the wire, never appears in
``repr`` and is overwritten when the credential is closed.
"""

from __future__ import annotations

import hmac
import re
import secrets
from typing import Optional

import blake3

from .errors import OgunUnauthenticated

TOKEN_PREFIX = "ot1_"
AUTH_KEY_CONTEXT = "ogun 2026-10 client auth key v1"
SERVER_LABEL = b"ogun-server-v1"
CLIENT_LABEL = b"ogun-client-v1"
KINDS = ("human", "agent", "service", "bridge")

_HEX64 = re.compile(r"^[0-9a-f]{64}$")
_NAME = re.compile(r"^[a-z0-9_-]{1,64}$")
_CREDENTIAL_ID = re.compile(r"^ocl_[0-7][0-9A-HJKMNP-TV-Z]{25}$")


def new_nonce() -> str:
    """32 random bytes as 64 lowercase hex digits."""
    return secrets.token_bytes(32).hex()


def _parse_hex32(text: str) -> Optional[bytearray]:
    text = text.strip()
    if not _HEX64.match(text):
        return None
    return bytearray(bytes.fromhex(text))


def _wipe(buf: bytearray) -> None:
    for i in range(len(buf)):
        buf[i] = 0


class IpcSecret:
    """The per-user secret that proves the endpoint is this user's ``ogund``."""

    __slots__ = ("_key",)

    def __init__(self, key: bytearray) -> None:
        self._key = key

    @classmethod
    def from_file_text(cls, text: str) -> "IpcSecret":
        """Parses the file form: 64 hex digits."""
        key = _parse_hex32(text)
        if key is None:
            raise OgunUnauthenticated("api.unauthenticated", "ipc secret file is malformed")
        return cls(key)

    @classmethod
    def read(cls, path: str) -> "IpcSecret":
        """Reads ``ipc.secret``."""
        try:
            with open(path, "r", encoding="ascii") as f:
                return cls.from_file_text(f.read())
        except OSError as e:
            from .errors import OgunUnavailable

            raise OgunUnavailable("api.unavailable", f"cannot read the ipc secret: {type(e).__name__}") from None

    def server_proof(self, client_nonce: str) -> str:
        """The proof a genuine daemon sends for ``client_nonce``."""
        return blake3.blake3(SERVER_LABEL + client_nonce.encode("ascii"), key=bytes(self._key)).hexdigest()

    def verify_server_proof(self, client_nonce: str, proof: str) -> bool:
        """Constant-time check of the daemon's proof."""
        if not isinstance(proof, str) or not _HEX64.match(proof):
            return False
        return hmac.compare_digest(self.server_proof(client_nonce), proof)

    def wipe(self) -> None:
        """Overwrites the key in memory."""
        _wipe(self._key)

    def __repr__(self) -> str:
        return "IpcSecret([REDACTED])"


class Credential:
    """A local credential: id, kind, name and token. File form: ``<ocl_id> <kind> <name> ot1_<hex>``."""

    __slots__ = ("id", "kind", "name", "_token")

    def __init__(self, credential_id: str, kind: str, name: str, token: bytearray) -> None:
        self.id = credential_id
        self.kind = kind
        self.name = name
        self._token = token

    @classmethod
    def from_file_text(cls, text: str) -> "Credential":
        """Parses one credential file."""
        parts = text.split()
        bad = OgunUnauthenticated("api.unauthenticated", "credential file is malformed")
        if len(parts) != 4:
            raise bad
        cid, kind, name, token = parts
        if not _CREDENTIAL_ID.match(cid) or kind not in KINDS or not _NAME.match(name):
            raise bad
        if not token.startswith(TOKEN_PREFIX):
            raise bad
        raw = _parse_hex32(token[len(TOKEN_PREFIX):])
        if raw is None:
            raise bad
        return cls(cid, kind, name, raw)

    @classmethod
    def read(cls, path: str) -> "Credential":
        """Reads a token file. Errors never include the file's contents."""
        try:
            with open(path, "r", encoding="ascii") as f:
                text = f.read()
        except OSError as e:
            raise OgunUnauthenticated(
                "api.unauthenticated", f"cannot read the credential file: {type(e).__name__}"
            ) from None
        return cls.from_file_text(text)

    def auth_key(self) -> bytes:
        """The key both sides derive from the token."""
        return blake3.blake3(bytes(self._token), derive_key_context=AUTH_KEY_CONTEXT).digest()

    def proof(self, server_nonce: str, client_nonce: str) -> str:
        """The client's proof for one connection."""
        msg = CLIENT_LABEL + self.id.encode("ascii") + server_nonce.encode("ascii") + client_nonce.encode("ascii")
        return blake3.blake3(msg, key=self.auth_key()).hexdigest()

    def wipe(self) -> None:
        """Overwrites the token in memory."""
        _wipe(self._token)

    def __repr__(self) -> str:
        return f"Credential(id={self.id!r}, kind={self.kind!r}, name={self.name!r}, token=[REDACTED])"

    __str__ = __repr__
