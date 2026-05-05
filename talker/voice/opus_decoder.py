import logging
import os
import sys
from pathlib import Path

# Make libopus visible to opuslib before its import. opuslib calls
# ctypes.util.find_library('opus') which is platform-specific:
#   - Windows: scans PATH; we ship a bundled opus.dll under native/win32/.
#   - macOS:   ctypes reads DYLD_FALLBACK_LIBRARY_PATH at call time. Homebrew
#              installs libopus to /opt/homebrew/lib (Apple Silicon) or
#              /usr/local/lib (Intel). The Intel path is in the dyld default
#              fallback, but the Apple Silicon path is not — append it.
#   - Linux:   the system loader resolves libopus.so via ldconfig; no
#              bootstrap needed if opus is installed via apt/pacman/etc.
if sys.platform == "win32":
    _dll_dir = Path(__file__).parent.parent / "native" / "win32"
    if _dll_dir.exists():
        os.environ["PATH"] = str(_dll_dir) + os.pathsep + os.environ.get("PATH", "")
        if hasattr(os, "add_dll_directory"):
            os.add_dll_directory(str(_dll_dir))
elif sys.platform == "darwin":
    _brew_lib_dirs = ["/opt/homebrew/lib", "/usr/local/lib"]
    _existing = os.environ.get("DYLD_FALLBACK_LIBRARY_PATH", "")
    _extra = [d for d in _brew_lib_dirs if d not in _existing.split(":")]
    if _extra:
        os.environ["DYLD_FALLBACK_LIBRARY_PATH"] = (
            ":".join(_extra + ([_existing] if _existing else []))
        )

import opuslib

logger = logging.getLogger(__name__)


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
        try:
            return self._decoder.decode(opus_frame, frame_size=320)
        except opuslib.OpusError:
            logger.debug("Corrupted Opus frame (%d bytes), dropping", len(opus_frame))
            return b""
