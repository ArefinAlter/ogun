import pytest

from ogun_client import OgunError
from ogun_client.transport import Transport


@pytest.mark.parametrize("chunks", [[b"12345\n"], [b"123", b"45\n"], [b"12345"]])
def test_response_limit_applies_before_returning_line(chunks):
    reads = iter(chunks)
    closed = []
    transport = Transport(lambda n: next(reads, b""), lambda data: None, lambda: closed.append(True))
    with pytest.raises(OgunError, match="response longer"):
        transport.read_line(limit=4)
    assert transport.closed
    assert closed == [True]


def test_limit_applies_per_line_and_accepts_exact_limit():
    reads = iter([b"1234\nabcd\n"])
    transport = Transport(lambda n: next(reads, b""), lambda data: None, lambda: None)
    assert transport.read_line(limit=4) == b"1234"
    assert transport.read_line(limit=4) == b"abcd"
