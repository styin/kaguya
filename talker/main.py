"""main.py — Starts Listener gRPC server + Talker gRPC server in same asyncio loop."""

import asyncio
import logging

import grpc

from proto import kaguya_pb2_grpc  # type: ignore[import]

logger = logging.getLogger(__name__)


def _bind_or_raise(server: grpc.aio.Server, addr: str, name: str) -> int:
    """Bind `server` on `addr` and fail loudly if the bind didn't take.

    `grpc.aio.Server.add_insecure_port()` returns the chosen port on
    success and `0` on failure (address-in-use, permission denied, malformed
    address). The original code ignored the return value and logged
    "listening" regardless — bind failures showed up later as opaque
    "connection refused" errors at the client. This helper makes the
    failure mode obvious: process exits with the offending address in
    the message, no more "but the log said it was listening!" debugging.
    """
    port = server.add_insecure_port(addr)
    if port == 0:
        raise RuntimeError(
            f"Failed to bind {name} gRPC on {addr!r} "
            "(address in use, permission denied, or invalid format)"
        )
    return port


async def main() -> None:
    # Heavy deps imported lazily so `from main import _bind_or_raise` (used
    # by tests/test_main_bind.py) doesn't transitively pull in RealtimeTTS
    # — the CI test environment intentionally omits RealtimeSTT/RealtimeTTS
    # to skip CUDA setup. Real runs always reach this code path and import
    # everything normally.
    from config import TalkerConfig
    from server import TalkerServiceServicer
    from voice.listener import Listener, ListenerServiceImpl
    from voice.speaker import Speaker

    config = TalkerConfig()
    logging.basicConfig(
        level=getattr(logging, config.log_level.upper(), logging.INFO),
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    logger.info("Talker Agent starting (LLM: %s)", config.llm_base_url)

    # Init components
    speaker = Speaker(config)
    talker_servicer = TalkerServiceServicer(config, speaker)
    listener = Listener(config)
    listener_servicer = ListenerServiceImpl(listener.event_queue)

    # ── Talker gRPC server ──
    talker_server = grpc.aio.server()
    kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(talker_servicer, talker_server)
    talker_port = _bind_or_raise(talker_server, config.talker_listen_addr, "Talker")
    await talker_server.start()
    logger.info(
        "Talker gRPC listening on %s (port=%d)", config.talker_listen_addr, talker_port
    )

    # ── Listener gRPC server (Gateway connects to us as client) ──
    listener_server = grpc.aio.server()
    kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(
        listener_servicer, listener_server
    )
    listener_port = _bind_or_raise(
        listener_server, config.listener_grpc_addr, "Listener"
    )
    await listener_server.start()
    logger.info(
        "Listener gRPC listening on %s (port=%d)",
        config.listener_grpc_addr,
        listener_port,
    )

    # ── Start Listener (audio socket + RealtimeSTT) ──
    listener_task = asyncio.create_task(listener.run())

    try:
        await talker_server.wait_for_termination()
    except asyncio.CancelledError:
        pass
    finally:
        listener_task.cancel()
        await asyncio.gather(listener_task, return_exceptions=True)
        await talker_servicer.close()
        await talker_server.stop(grace=2.0)
        await listener_server.stop(grace=2.0)
        logger.info("Talker Agent shut down")


if __name__ == "__main__":
    asyncio.run(main())
