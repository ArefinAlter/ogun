"""One HTTP POST (or PUT) with retries, and nothing that can leak.

Error messages carry the HTTP status, the sink name and the URL without userinfo, query or
fragment. They never carry a request body, a header, a secret or a response body. A URL that is
itself a secret (a chat webhook) is shown without its path as well.
"""

from __future__ import annotations

import http.client
import ipaddress
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Callable, Dict, Optional

from . import __version__

MAX_RETRY_AFTER_S = 60.0
_BACKOFF_BASE_S = 0.5


class DeliveryError(Exception):
    """A delivery failed. The message is safe to log."""

    def __init__(self, message: str, *, status: Optional[int] = None, sink: str = "", delivered: int = 0) -> None:
        super().__init__(message)
        #: The HTTP status, when there was a response.
        self.status = status
        self.sink = sink
        #: How many leading items of a batch were delivered before the failure.
        self.delivered = delivered


def redact_url(url: str, keep_path: bool = True) -> str:
    """The URL without userinfo, query and fragment; without its path too when ``keep_path`` is false."""
    try:
        parts = urllib.parse.urlsplit(url)
        host = parts.hostname or ""
        port = parts.port
    except ValueError:
        return "<invalid url>"
    if not parts.scheme or not host:
        return "<invalid url>"
    netloc = f"[{host}]" if ":" in host else host
    if port:
        netloc += f":{port}"
    path = parts.path
    if not keep_path and path not in ("", "/"):
        path = "/..."
    return urllib.parse.urlunsplit((parts.scheme, netloc, path, "", ""))


def is_loopback(host: str) -> bool:
    """Whether a host name is ``localhost`` or a loopback address."""
    if host.lower() == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def url_problem(url: str, allow_insecure_http: bool = False) -> Optional[str]:
    """Why a URL may not be used, or ``None``. Plain ``http`` is for loopback hosts only, unless allowed."""
    try:
        parts = urllib.parse.urlsplit(url)
        host = parts.hostname
        parts.port  # noqa: B018 - raises ValueError for a bad port
    except ValueError:
        return "the URL is not valid"
    if parts.scheme not in ("http", "https"):
        return "only http and https URLs are allowed"
    if not host:
        return "the URL has no host"
    if parts.scheme == "http" and not is_loopback(host) and not allow_insecure_http:
        return "plain http is allowed only for loopback hosts (set allow_insecure_http to override)"
    return None


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Redirects are not followed: a redirect would resend headers, which may carry a secret."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[no-untyped-def]
        return None


def _opener(url: str) -> urllib.request.OpenerDirector:
    host = urllib.parse.urlsplit(url).hostname or ""
    if is_loopback(host):
        return urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirect)
    return urllib.request.build_opener(_NoRedirect)


def _retry_after(value: Optional[str]) -> Optional[float]:
    if not value:
        return None
    try:
        seconds = float(value.strip())
    except ValueError:
        return None
    if seconds < 0 or seconds != seconds:
        return None
    return min(seconds, MAX_RETRY_AFTER_S)


def post(
    url: str,
    body: bytes,
    headers: Dict[str, str],
    timeout: float,
    method: str = "POST",
    *,
    sink: str = "",
    sleep: Callable[[float], None] = time.sleep,
    allow_insecure_http: bool = False,
    secret_url: bool = False,
    retries: int = 3,
) -> bytes:
    """Sends ``body`` and returns the response body (never logged).

    Connection errors, 429 and 5xx are retried ``retries`` times with exponential backoff
    (0.5 s, 1 s, 2 s); ``Retry-After`` is honoured, up to 60 s. Other 4xx and redirects are not
    retried. Raises :class:`DeliveryError`.
    """
    shown = redact_url(url, keep_path=not secret_url)
    problem = url_problem(url, allow_insecure_http)
    if problem:
        raise DeliveryError(f'delivery to sink "{sink}" refused: {problem} ({shown})', sink=sink)
    opener = _opener(url)
    attempts = retries + 1
    failure = ""
    status: Optional[int] = None
    made = 0
    for attempt in range(attempts):
        made += 1
        delay = _BACKOFF_BASE_S * (2**attempt)
        retryable = False
        result: Optional[bytes] = None
        failure = ""
        status = None
        try:
            req = urllib.request.Request(url, data=body, method=method)
            req.add_header("User-Agent", f"ogun-export/{__version__}")
            for name, value in headers.items():
                req.add_header(name, value)
            with opener.open(req, timeout=timeout) as resp:
                result = resp.read()
        except urllib.error.HTTPError as e:
            status = e.code
            if status == 429 or status >= 500:
                retryable = True
                hinted = _retry_after(e.headers.get("Retry-After") if e.headers else None)
                if hinted is not None:
                    delay = hinted
                failure = f"HTTP {status}"
            elif 300 <= status < 400:
                failure = f"HTTP {status} (redirects are not followed)"
            else:
                failure = f"HTTP {status}"
            try:
                e.read()
            except Exception:  # noqa: BLE001 - the body is discarded either way
                pass
            e.close()
        except (urllib.error.URLError, OSError, http.client.HTTPException) as e:
            retryable = True
            inner = e.reason if isinstance(e, urllib.error.URLError) and isinstance(e.reason, BaseException) else e
            failure = f"connection error ({type(inner).__name__})"
        except ValueError:
            # http.client rejects some header values with a message that repeats the value.
            failure = "the request was rejected locally (invalid header or URL)"
        if result is not None:
            return result
        if not retryable or attempt == attempts - 1:
            break
        sleep(delay)
    tail = f" after {made} attempts" if made > 1 else ""
    raise DeliveryError(f'delivery to sink "{sink}" failed: {failure}{tail} ({shown})', status=status, sink=sink)
