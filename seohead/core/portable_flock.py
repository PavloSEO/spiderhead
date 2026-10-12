"""``fcntl``-compatible advisory file locks for the project stores on every platform.

POSIX returns the real ``fcntl`` module. Windows has none, so ``flock`` is mapped onto
``msvcrt.locking`` over the first byte: exclusive and shared requests both take the
byte-range lock (blocking, retried by the CRT) and ``LOCK_UN`` releases it.
"""

from __future__ import annotations

import os

try:
    import fcntl as _fcntl
except ImportError:  # Windows
    _fcntl = None


class _WindowsFlock:
    LOCK_SH = 1
    LOCK_EX = 2
    LOCK_UN = 8

    @staticmethod
    def flock(descriptor: int, operation: int) -> None:
        import msvcrt

        position = os.lseek(descriptor, 0, os.SEEK_CUR)
        try:
            os.lseek(descriptor, 0, os.SEEK_SET)
            if operation & _WindowsFlock.LOCK_UN:
                msvcrt.locking(descriptor, msvcrt.LK_UNLCK, 1)
            else:
                msvcrt.locking(descriptor, msvcrt.LK_LOCK, 1)
        finally:
            os.lseek(descriptor, position, os.SEEK_SET)


fcntl = _fcntl if _fcntl is not None else _WindowsFlock
