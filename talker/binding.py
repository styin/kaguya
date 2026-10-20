"""Single instance/session binding for the shared voice process.

The first valid RPC binds the process to its instance/session metadata. Talker,
Listener gRPC, and raw audio ingress share this guard because the process has one
persona, speaker, and Listener queue. Later RPCs validate the existing binding;
accepting a different pair requires explicit resource multiplexing.
"""

import asyncio
import uuid

import grpc

INSTANCE_HEADER = "x-kaguya-instance-id"
SESSION_HEADER = "x-kaguya-session-id"
AUDIO_MAGIC = b"KAGUYA1"


class InstanceSessionBinding:
    def __init__(self) -> None:
        self._binding: tuple[str, str] | None = None
        self._lock = asyncio.Lock()

    async def bind_or_validate(self, context: grpc.aio.ServicerContext) -> str:
        """Bind the first valid pair or require the current pair; return its session."""
        metadata = dict(context.invocation_metadata())
        instance_id = str(metadata.get(INSTANCE_HEADER, ""))
        session_id = str(metadata.get(SESSION_HEADER, ""))
        try:
            instance = uuid.UUID(instance_id)
            session = uuid.UUID(session_id)
            if (
                not instance_id
                or not session_id
                or instance.int == 0
                or session.int == 0
                or instance == session
                or str(instance) != instance_id
                or str(session) != session_id
            ):
                raise ValueError("invalid instance/session identifiers")
        except (ValueError, AttributeError):
            await context.abort(
                grpc.StatusCode.INVALID_ARGUMENT,
                "missing or invalid instance/session metadata",
            )

        requested = (instance_id, session_id)
        async with self._lock:
            if self._binding is None:
                self._binding = requested
            elif self._binding != requested:
                await context.abort(
                    grpc.StatusCode.FAILED_PRECONDITION,
                    "voice process is bound to a different instance/session",
                )
        return session_id

    def matches_audio_header(self, header: bytes) -> bool:
        if len(header) != len(AUDIO_MAGIC) + 32 or not header.startswith(AUDIO_MAGIC):
            return False
        if self._binding is None:
            return False
        instance = str(
            uuid.UUID(bytes=header[len(AUDIO_MAGIC) : len(AUDIO_MAGIC) + 16])
        )
        session = str(uuid.UUID(bytes=header[len(AUDIO_MAGIC) + 16 :]))
        return (instance, session) == self._binding
