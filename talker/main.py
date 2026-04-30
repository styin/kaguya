"""main.py — Starts Listener gRPC server + Talker gRPC server in same asyncio loop."""

import asyncio
import logging

import grpc

from config import TalkerConfig
from server import TalkerServiceServicer
from voice.listener import Listener, ListenerServiceImpl
from voice.speaker import Speaker

from proto import kaguya_pb2_grpc  # type: ignore[import]

logger = logging.getLogger(__name__)


async def main() -> None:
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
    talker_server.add_insecure_port(config.talker_listen_addr)
    await talker_server.start()
    logger.info("Talker gRPC listening on %s", config.talker_listen_addr)

    # ── Listener gRPC server (Gateway connects to us as client) ──
    listener_server = grpc.aio.server()
    kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(
        listener_servicer, listener_server
    )
    listener_server.add_insecure_port(config.listener_grpc_addr)
    await listener_server.start()
    logger.info("Listener gRPC listening on %s", config.listener_grpc_addr)

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