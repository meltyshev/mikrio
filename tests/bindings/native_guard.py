"""Independent, nonblocking OS guard probe for installed binding tests."""

from __future__ import annotations

import errno
import sys
from typing import BinaryIO


def lock(guard: BinaryIO) -> None:
    if sys.platform != "win32":
        import fcntl

        fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return

    import msvcrt

    # The first byte overlaps the engine's whole-file LockFileEx guard. Locking
    # past the empty file's EOF neither extends the file nor changes its bytes.
    guard.seek(0)

    try:
        msvcrt.locking(guard.fileno(), msvcrt.LK_NBLCK, 1)
    except OSError as error:
        if error.errno == errno.EACCES:
            raise BlockingIOError(errno.EACCES, "native guard is held") from error

        raise


def unlock(guard: BinaryIO) -> None:
    if sys.platform != "win32":
        import fcntl

        fcntl.flock(guard, fcntl.LOCK_UN)
        return

    import msvcrt

    guard.seek(0)
    msvcrt.locking(guard.fileno(), msvcrt.LK_UNLCK, 1)


def main() -> int:
    action, path = sys.argv[1:]

    if action not in {"hold", "probe"}:
        raise ValueError("expected hold or probe")

    with open(path, "r+b") as guard:
        try:
            lock(guard)
        except BlockingIOError:
            return 1

        try:
            if action == "hold":
                print("held", flush=True)
                sys.stdin.readline()
        finally:
            unlock(guard)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
