"""Immutable expected scope for the dedicated voice process.

The launcher supplies this pair before any service starts. Talker, Listener gRPC,
and raw audio ingress share the same value; a request can assert the configured
scope but cannot claim or rebind the process.
"""

import uuid
from dataclasses import dataclass

import grpc

from proto import kaguya_pb2

AUDIO_MAGIC = b"KAGUYA1"


def validate_identifiers(
    instance_id: str | None, session_id: str | None
) -> tuple[str, str]:
    """Require canonical, non-nil, distinct UUIDs; never generate replacements."""
    try:
        if not instance_id or not session_id:
            raise ValueError("missing identifiers")
        instance = uuid.UUID(instance_id)
        session = uuid.UUID(session_id)
        if (
            instance.int == 0
            or session.int == 0
            or instance == session
            or str(instance) != instance_id
            or str(session) != session_id
        ):
            raise ValueError("invalid identifiers")
    except (ValueError, AttributeError, TypeError) as exc:
        raise ValueError(
            "KAGUYA_INSTANCE_ID and KAGUYA_SESSION_ID must be canonical, "
            "non-nil, distinct UUIDs"
        ) from exc
    return instance_id, session_id


@dataclass(frozen=True)
class InstanceSessionBinding:
    instance_id: str
    session_id: str

    def __post_init__(self) -> None:
        validate_identifiers(self.instance_id, self.session_id)

    async def require(
        self, session: kaguya_pb2.SessionRef, context: grpc.aio.ServicerContext
    ) -> None:
        """Validate a protobuf assertion without changing the expected pair."""
        try:
            if not session.HasField("instance_id") or not session.HasField(
                "session_id"
            ):
                raise ValueError("SessionRef requires both ID objects")
            requested = validate_identifiers(
                session.instance_id.value, session.session_id.value
            )
        except ValueError:
            await context.abort(
                grpc.StatusCode.INVALID_ARGUMENT,
                "missing or invalid SessionRef",
            )
            return
        if requested != (self.instance_id, self.session_id):
            await context.abort(
                grpc.StatusCode.FAILED_PRECONDITION,
                "SessionRef does not match the configured voice scope",
            )

    def matches_audio_header(self, header: bytes) -> bool:
        if len(header) != len(AUDIO_MAGIC) + 32 or not header.startswith(AUDIO_MAGIC):
            return False
        instance = str(
            uuid.UUID(bytes=header[len(AUDIO_MAGIC) : len(AUDIO_MAGIC) + 16])
        )
        session = str(uuid.UUID(bytes=header[len(AUDIO_MAGIC) + 16 :]))
        return (instance, session) == (self.instance_id, self.session_id)
