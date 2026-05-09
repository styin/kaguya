"""P0-5: Talker startup must fail loudly on gRPC bind failure.

`grpc.aio.Server.add_insecure_port(addr)` returns the bound port number,
or `0` if the bind failed. The original main.py ignored the return value
and proceeded to log "listening" regardless — which silently hid bind
failures (e.g. address-in-use, permission denied) and caused puzzling
"connection refused" symptoms downstream.

The fix introduces a `_bind_or_raise(server, addr, name)` helper that
returns the port on success and raises RuntimeError on bind=0. These
tests pin the helper's contract.
"""

from unittest.mock import MagicMock

import pytest


def test_bind_or_raise_returns_port_on_success():
    """Successful bind: returns the port that gRPC actually bound."""
    from main import _bind_or_raise  # type: ignore[import]

    server = MagicMock()
    server.add_insecure_port = MagicMock(return_value=50053)

    port = _bind_or_raise(server, "0.0.0.0:50053", "Talker")
    assert port == 50053
    server.add_insecure_port.assert_called_once_with("0.0.0.0:50053")


def test_bind_or_raise_raises_on_zero_return():
    """Bind failure (port=0): raise RuntimeError with the address in the
    message so logs immediately point at the misconfiguration."""
    from main import _bind_or_raise  # type: ignore[import]

    server = MagicMock()
    server.add_insecure_port = MagicMock(return_value=0)

    with pytest.raises(RuntimeError, match=r"0\.0\.0\.0:50055"):
        _bind_or_raise(server, "0.0.0.0:50055", "Listener")


def test_bind_or_raise_error_includes_service_name():
    """The error message must name which service failed (Talker vs.
    Listener) so the operator can find it in code without grepping ports."""
    from main import _bind_or_raise  # type: ignore[import]

    server = MagicMock()
    server.add_insecure_port = MagicMock(return_value=0)

    with pytest.raises(RuntimeError, match=r"Listener"):
        _bind_or_raise(server, "127.0.0.1:50055", "Listener")
