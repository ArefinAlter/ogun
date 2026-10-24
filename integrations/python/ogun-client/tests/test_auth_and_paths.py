"""Vectors produced by the Rust ogun-api crate (ogun_api::auth and ogun_api::endpoint), so the
Python handshake and endpoint discovery match the daemon byte for byte."""

from ogun_client.auth import Credential, IpcSecret
from ogun_client.paths import OgunPaths

CLIENT_NONCE = "22" * 32
SERVER_NONCE = "33" * 32


def test_handshake_matches_rust():
    secret = IpcSecret.from_file_text("11" * 32 + "\n")
    assert secret.server_proof(CLIENT_NONCE) == "f35a0aec069790999182f7b097b2ba2c248df5e6ddf912a187a5d1066ee12be9"
    assert secret.verify_server_proof(CLIENT_NONCE, "f35a0aec069790999182f7b097b2ba2c248df5e6ddf912a187a5d1066ee12be9")
    assert not secret.verify_server_proof(CLIENT_NONCE, "00" * 32)
    cred = Credential.from_file_text("ocl_01J9Z3K4M5N6P7Q8R9S0T1V2W3 agent mcp ot1_" + "44" * 32 + "\n")
    assert cred.proof(SERVER_NONCE, CLIENT_NONCE) == "cb7cab31de088b55232a317e7621d5d7452071b6706711a2993bea0e6fb9ddb2"
    assert "44" * 8 not in repr(cred)
    cred.wipe()
    assert cred.proof(SERVER_NONCE, CLIENT_NONCE) != "cb7cab31de088b55232a317e7621d5d7452071b6706711a2993bea0e6fb9ddb2"


def test_malformed_credentials_are_refused_without_echo():
    import pytest

    from ogun_client import OgunUnauthenticated

    for text in ["", "ocl_x agent mcp ot1_00", "ocl_01J9Z3K4M5N6P7Q8R9S0T1V2W3 root mcp ot1_" + "44" * 32]:
        with pytest.raises(OgunUnauthenticated) as e:
            Credential.from_file_text(text)
        assert "44" * 4 not in str(e.value)


def test_endpoints_match_rust():
    assert str(OgunPaths.rooted("/tmp/ogun-home", windows=False).endpoint()) == "/tmp/ogun-home/run/ogun/ogund.sock"
    assert (
        str(OgunPaths.rooted("/tmp/ogun-home", "edge", windows=False).endpoint())
        == "/tmp/ogun-home/profiles/edge/run/ogun/ogund-edge.sock"
    )
    long = OgunPaths.rooted("/tmp/" + "x" * 120, windows=False)
    assert str(long.endpoint(temp_dir="/tmp")) == "/tmp/ogun-5b1078044704/ogund.sock"
    env = {"HOME": "/home/u", "XDG_STATE_HOME": "/home/u/.local/state", "XDG_RUNTIME_DIR": "/run/user/1000"}
    p = OgunPaths.discover("lab", env=env, platform="linux")
    assert p.state_dir == "/home/u/.local/state/ogun/profiles/lab"
    assert str(p.endpoint()) == "/run/user/1000/ogun/ogund-lab.sock"
    p = OgunPaths.discover("lab", env={"HOME": "/root"}, platform="linux")
    assert str(p.endpoint()) == "/root/.local/state/ogun/profiles/lab/run/ogun/ogund-lab.sock"
    w = OgunPaths.discover(None, env={"LOCALAPPDATA": "C:\\Users\\u\\AppData\\Local"}, platform="win32")
    assert w.state_dir == "C:\\Users\\u\\AppData\\Local\\Ogun"
    assert w.endpoint().kind == "pipe" and w.endpoint().address.startswith("\\\\.\\pipe\\ogund-")
    assert OgunPaths.discover(None, env={"OGUN_HOME": "/srv/o"}, platform="linux").state_dir == "/srv/o"
