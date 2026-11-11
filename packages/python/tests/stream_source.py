"""Producer adapter for the native stream ownership fixture."""

import sys
import time
from collections.abc import Iterable, Iterator
from typing import Protocol

from _mikrio_stream_probe import StreamProbe

CHUNK_BYTES = 64 * 1024


class BinaryReader(Protocol):
    def read(self, size: int, /) -> bytes: ...


class ProducerFailure(Exception):
    code = "BodyReadFailed"


def read_file_chunks(reader: BinaryReader) -> Iterator[bytes]:
    while chunk := reader.read(CHUNK_BYTES):
        yield chunk


def produce(stream: StreamProbe, source: Iterable[bytes | bytearray]) -> None:
    iterator = iter(source)
    finished = False

    try:
        while True:
            # Reserve before calling a producer, including potentially blocked
            # callbacks. No foreign buffer enters the native executor.
            reservation = stream.reserve()

            try:
                chunk = next(iterator)
            except StopIteration:
                stream.end()
                finished = True
                return

            if not isinstance(chunk, bytes | bytearray):
                raise TypeError("producer must supply a binary chunk")

            with memoryview(chunk) as view:
                if not view:
                    reservation.write(b"")

                for offset in range(0, len(view), CHUNK_BYTES):
                    if offset:
                        reservation = stream.reserve()

                    # Mutable storage is copied while Python owns it, bounded
                    # by one chunk, then copied into the reserved Rust owner.
                    reservation.write(bytes(view[offset : offset + CHUNK_BYTES]))

            time.sleep(0)  # Endless empty sources still yield and check signals.
    except (KeyboardInterrupt, SystemExit):
        stream.close()
        raise
    except Exception as cause:
        stream.fail_producer()
        raise ProducerFailure("Producer failed") from cause
    finally:
        if not finished:
            close = getattr(iterator, "close", None)

            if close is not None:
                primary = sys.exception()

                try:
                    close()
                except BaseException as cleanup:
                    # Cleanup must not replace a producer/control-flow error.
                    if primary is None:
                        raise

                    cleanup.__context__ = None
                    primary.__context__ = cleanup
