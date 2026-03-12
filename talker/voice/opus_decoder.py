import os
import sys
from pathlib import Path

# On Windows, make the bundled opus.dll visible to opuslib before import.
# opuslib calls ctypes.util.find_library('opus') which scans PATH, then loads
# the result with ctypes.CDLL. Both steps need the directory:
#   - PATH: so find_library can locate opus.dll
#   - add_dll_directory: so ctypes.CDLL can resolve its dependencies
if sys.platform == "win32":
    _dll_dir = Path(__file__).parent.parent / "native" / "win32"
    if _dll_dir.exists():
        os.environ["PATH"] = str(_dll_dir) + os.pathsep + os.environ.get("PATH", "")
        if hasattr(os, "add_dll_directory"):
            os.add_dll_directory(str(_dll_dir))

import opuslib


class OpusDecoder:
    """Decodes Opus frames to 16kHz mono 16-bit PCM.

    libopus handles internal resampling, so we decode directly to 16kHz —
    no separate downsample step. See REF-002 for full rationale.
    """

    def __init__(self) -> None:
        self._decoder = opuslib.Decoder(fs=16000, channels=1)

    def decode(self, opus_frame: bytes) -> bytes:
        """Decode one Opus frame to raw 16-bit signed PCM.

        Args:
            opus_frame: 20ms Opus-encoded audio frame.

        Returns:
            Raw 16-bit signed PCM bytes at 16kHz mono, ready for
            RealtimeSTT.feed_audio(). frame_size=320 = 16000 × 0.02s.
        """
        return self._decoder.decode(opus_frame, frame_size=320)
