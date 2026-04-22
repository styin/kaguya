"""main.py — Talker Agent asyncio entrypoint.

Starts the gRPC TalkerService server and the Listener task in the same
asyncio event loop (shared Python process for GPU context sharing).
"""

import asyncio
import logging

import grpc

from config import TalkerConfig
from server import TalkerServiceServicer
from voice.listener import Listener
from voice.speaker import Speaker

from proto import kaguya_pb2_grpc  # type: ignore[import]

logger = logging.getLogger(__name__)


def _talker_bind_addr_for_temp_tcp(raw: str) -> str:
    """Temporary Windows-safe routing: divert unix-style bind targets to TCP."""
    value = raw.strip()
    if "://" in value and not value.startswith(("http://", "https://", "dns://")):
        logger.warning(
            "Temporary TCP fallback: ignoring non-TCP Talker bind target '%s', using 0.0.0.0:50053",
            value,
        )
        return "0.0.0.0:50053"

    if "/" in value or "\\" in value:
        logger.warning(
            "Temporary TCP fallback: ignoring path-like Talker bind target '%s', using 0.0.0.0:50053",
            value,
        )
        return "0.0.0.0:50053"

    return value


async def main() -> None:
    config = TalkerConfig()

    # Configure logging.
    logging.basicConfig(
        level=getattr(logging, config.log_level.upper(), logging.INFO),
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    logger.info("Talker Agent starting (LLM: %s)", config.llm_base_url)

    # Init components.
    speaker = Speaker(config)
    servicer = TalkerServiceServicer(config, speaker)
    listener = Listener(config)

    # Start gRPC server.
    server = grpc.aio.server()
    kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(servicer, server)
    socket_addr = _talker_bind_addr_for_temp_tcp(config.talker_listen_addr)
    server.add_insecure_port(socket_addr)
    await server.start()
    logger.info("gRPC TalkerService listening on %s", socket_addr)

    # TODO: Wire audio input to listener.feed_opus(). Needs either:
    #   - A raw Unix socket reader task that reads Opus frames from Gateway
    #   - A dev-mode microphone capture task for local testing without Gateway
    # Until then, listener.run() starts but receives no audio frames.

    # Run Listener and gRPC server concurrently.
    listener_task = asyncio.create_task(listener.run())
    try:
        await server.wait_for_termination()
    except asyncio.CancelledError:
        pass
    finally:
        listener_task.cancel()
        await asyncio.gather(listener_task, return_exceptions=True)
        await servicer.close()
        await server.stop(grace=2.0)
        logger.info("Talker Agent shut down")


if __name__ == "__main__":
    asyncio.run(main())
