"""Fixed deployment scope used by voice protocol tests."""

import pytest

from binding import InstanceSessionBinding
from proto import kaguya_pb2

INSTANCE_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
SESSION_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"


@pytest.fixture
def session_ref_factory():
    def create(instance_id=INSTANCE_ID, session_id=SESSION_ID):
        return kaguya_pb2.SessionRef(
            instance_id=kaguya_pb2.InstanceId(value=instance_id),
            session_id=kaguya_pb2.SessionId(value=session_id),
        )

    return create


@pytest.fixture
def session_ref(session_ref_factory):
    return session_ref_factory()


@pytest.fixture
def binding():
    return InstanceSessionBinding(INSTANCE_ID, SESSION_ID)
