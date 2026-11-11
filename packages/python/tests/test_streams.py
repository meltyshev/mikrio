"""Explicit instrumented extension tests; never evidence for ordinary wheels."""

from __future__ import annotations

import io
import os
import queue
import signal
import threading
import time
import tracemalloc
import unittest
from collections import Counter
from collections.abc import Callable, Iterator
from typing import TypeVar

from _mikrio_stream_probe import ProbeError, StreamProbe
from stream_source import CHUNK_BYTES, ProducerFailure, produce, read_file_chunks

T = TypeVar("T")


class StreamTests(unittest.TestCase):
    def create_stream(self, expected_length: int | None = None) -> StreamProbe:
        stream = StreamProbe(expected_length)
        self.addCleanup(stream.close)
        return stream

    def wait_until(self, predicate: Callable[[], bool]) -> None:
        deadline = time.monotonic() + 5

        while not predicate():
            self.assertLess(time.monotonic(), deadline, "native handshake timed out")
            time.sleep(0.001)

    def run_in_background(
        self, action: Callable[[], T], cancel: Callable[[], None] | None = None
    ) -> Callable[[], T]:
        outcomes: queue.Queue[tuple[bool, T | BaseException]] = queue.Queue()

        def run() -> None:
            try:
                outcomes.put((True, action()))
            except BaseException as error:
                # This is a thread outcome mailbox, not an error conversion.
                outcomes.put((False, error))

        thread = threading.Thread(target=run)
        thread.start()

        def join_worker() -> None:
            thread.join(5)
            self.assertFalse(thread.is_alive(), "fixture thread did not settle")

        self.addCleanup(join_worker)

        if cancel is not None:
            self.addCleanup(cancel)

        def result() -> T:
            succeeded, value = outcomes.get(timeout=5)
            join_worker()

            if not succeeded:
                assert isinstance(value, BaseException)
                raise value

            # Successful actions in this fixture do not return exception values.
            assert not isinstance(value, BaseException)
            return value

        return result

    def test_empty_chunks_eof_close_and_overlapping_reads(self) -> None:
        stream = self.create_stream(2)
        stream.reserve().write(b"")
        pending = self.run_in_background(lambda: stream.read_chunk(1), stream.close)
        self.wait_until(lambda: stream.is_reading)

        with self.assertRaisesRegex(ProbeError, "BodyInUse"):
            stream.read_chunk(1)

        stream.reserve().write(b"xy")
        self.assertEqual(pending(), b"x")
        stream.end()
        self.assertEqual(stream.read_all(1), b"y")
        self.assertIsNone(stream.read_chunk(1))
        self.assertIsNone(stream.read_chunk(1))
        self.assertEqual(stream.read_all(0), b"")
        stream.close()
        stream.close()

        with self.assertRaisesRegex(ProbeError, "BodyClosed"):
            stream.read_chunk(1)

        self.assertEqual(stream.allocations[0], 0)

    def test_backpressure_retains_full_partial_allocation_and_releases_gil(
        self,
    ) -> None:
        stream = self.create_stream()
        stream.reserve().write(b"a" * CHUNK_BYTES)
        stream.reserve().write(b"b" * CHUNK_BYTES)
        admitted = threading.Event()
        started = threading.Event()

        def reserve() -> None:
            started.set()
            reservation = stream.reserve()
            admitted.set()
            reservation.write(b"c")

        result = self.run_in_background(reserve, stream.close)
        self.assertTrue(started.wait(5))
        self.assertEqual(stream.read_chunk(1), b"a")
        self.assertFalse(admitted.wait(0.05))
        self.assertEqual(stream.allocations, (131072, 131072))
        self.assertEqual(stream.read_chunk(CHUNK_BYTES), b"a" * (CHUNK_BYTES - 1))
        result()
        stream.end()
        self.assertEqual(stream.read_all(CHUNK_BYTES + 1), b"b" * CHUNK_BYTES + b"c")
        self.assertEqual(stream.allocations, (0, 131072))

    def test_large_reused_buffers_and_file_reader_keep_bounded_native_queue(
        self,
    ) -> None:
        stream = self.create_stream(8 * 1024 * 1024 + 2 * CHUNK_BYTES)
        finalized = threading.Event()

        def source() -> Iterator[bytes | bytearray]:
            try:
                huge = bytearray(b"a" * (8 * 1024 * 1024))
                yield huge
                huge[:] = b"z" * len(huge)
                reused = bytearray(b"b" * CHUNK_BYTES)
                yield reused
                reused[:] = b"c" * CHUNK_BYTES
                yield reused
                reused[:] = b"z" * CHUNK_BYTES
            finally:
                finalized.set()

        producing = self.run_in_background(
            lambda: produce(stream, source()), stream.close
        )

        counts: Counter[int] = Counter()

        while (chunk := stream.read_chunk(8191)) is not None:
            self.assertLessEqual(len(chunk), 8191)
            counts.update(chunk)

        producing()

        self.assertEqual(
            counts, {97: 8 * 1024 * 1024, 98: CHUNK_BYTES, 99: CHUNK_BYTES}
        )

        self.assertTrue(finalized.is_set())
        self.assertLessEqual(stream.allocations[1], 131072)
        self.assertEqual(stream.allocations[0], 0)

        reader = io.BytesIO(b"file reader")
        self.addCleanup(reader.close)
        file_stream = self.create_stream()
        produce(file_stream, read_file_chunks(reader))
        self.assertEqual(file_stream.read_all(11), b"file reader")
        self.assertFalse(reader.closed)

    def test_producer_causes_and_control_flow_exceptions_preserve_identity(
        self,
    ) -> None:
        for cause in (ValueError("producer failure"), KeyboardInterrupt("interrupt")):
            with self.subTest(cause=type(cause).__name__):
                stream = self.create_stream()

                def source(cause: BaseException = cause) -> Iterator[bytes]:
                    yield b"x"
                    raise cause

                if isinstance(cause, KeyboardInterrupt):
                    with self.assertRaises(KeyboardInterrupt) as interrupted:
                        produce(stream, source())

                    self.assertIs(interrupted.exception, cause)
                else:
                    with self.assertRaises(ProducerFailure) as failed:
                        produce(stream, source())

                    self.assertIs(failed.exception.__cause__, cause)

                    with self.assertRaisesRegex(ProbeError, "BodyReadFailed"):
                        stream.read_chunk(1)

                with self.assertRaisesRegex(ProbeError, "BodyClosed"):
                    stream.read_chunk(1)

                self.assertEqual(stream.allocations[0], 0)

    def test_length_mismatch_and_read_all_limit_never_finish_successfully(self) -> None:
        short = self.create_stream(3)
        short.reserve().write(b"x")

        with self.assertRaisesRegex(ProbeError, "LengthMismatch"):
            short.end()

        with self.assertRaisesRegex(ProbeError, "LengthMismatch"):
            short.read_chunk(1)

        long = self.create_stream(1)

        with self.assertRaisesRegex(ProbeError, "LengthMismatch"):
            long.reserve().write(b"xy")

        with self.assertRaisesRegex(ProbeError, "LengthMismatch"):
            long.read_chunk(1)

        bounded = self.create_stream(2 * CHUNK_BYTES)
        bounded.reserve().write(b"x" * CHUNK_BYTES)
        bounded.reserve().write(b"y" * CHUNK_BYTES)

        with self.assertRaisesRegex(ProbeError, "BodyTooLarge"):
            bounded.read_all(17)

        with self.assertRaisesRegex(ProbeError, "BodyClosed"):
            bounded.read_chunk(1)

        for stream in (short, long, bounded):
            self.assertEqual(stream.allocations[0], 0)

    def test_read_all_fills_one_python_allocation_from_bounded_native_chunks(
        self,
    ) -> None:
        length = 4 * 1024 * 1024 + 17
        stream = self.create_stream(length)

        def source() -> Iterator[bytes]:
            chunk = b"a" * CHUNK_BYTES

            for _ in range(length // CHUNK_BYTES):
                yield chunk

            yield b"b" * 17

        tracemalloc.start()

        try:
            producing = self.run_in_background(
                lambda: produce(stream, source()), stream.close
            )

            result = stream.read_all(length)
            producing()
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()

        self.assertEqual(len(result), length)
        self.assertEqual(result.count(b"a"), length - 17)
        self.assertTrue(result.endswith(b"b" * 17))
        self.assertLess(peak, length + 1024 * 1024)
        self.assertLessEqual(stream.allocations[1], 131072)
        self.assertEqual(stream.allocations[0], 0)
        self.assertIsNone(stream.read_chunk(1))

    def test_close_returns_while_foreign_callback_is_stalled_and_refuses_late_bytes(
        self,
    ) -> None:
        stream = self.create_stream()
        entered, release, finalized = (threading.Event() for _ in range(3))

        def source() -> Iterator[bytes]:
            try:
                entered.set()

                if not release.wait(5):
                    raise TimeoutError("fixture callback release missing")

                yield b"late"
            finally:
                finalized.set()

        producing = self.run_in_background(
            lambda: produce(stream, source()), stream.close
        )

        self.addCleanup(release.set)
        self.assertTrue(entered.wait(5))
        reading = self.run_in_background(lambda: stream.read_chunk(1), stream.close)
        self.wait_until(lambda: stream.is_reading)
        stream.close()
        self.assertFalse(release.is_set())

        with self.assertRaisesRegex(ProbeError, "BodyClosed"):
            reading()

        release.set()

        with self.assertRaises(ProducerFailure) as failed:
            producing()

        self.assertIsInstance(failed.exception.__cause__, ProbeError)
        self.assertTrue(finalized.is_set())
        self.assertEqual(stream.allocations, (0, 0))

    def test_keyboard_interrupt_cancels_pending_native_read_and_preserves_exception(
        self,
    ) -> None:
        stream = self.create_stream()
        stop = threading.Event()

        def interrupt() -> None:
            self.wait_until(lambda: stop.is_set() or stream.is_reading)

            if not stop.is_set():
                os.kill(os.getpid(), signal.SIGINT)

        delivered = self.run_in_background(interrupt)
        self.addCleanup(stop.set)

        with self.assertRaises(KeyboardInterrupt):
            stream.read_chunk(1)

        delivered()
        self.wait_until(lambda: not stream.is_reading)

        with self.assertRaisesRegex(ProbeError, "BodyClosed"):
            stream.read_chunk(1)

    def test_invalid_lengths_limits_and_text_fail_without_copying(self) -> None:
        for expected_length in (-1, 1 << 64):
            with self.subTest(expected_length=expected_length):
                with self.assertRaises(OverflowError):
                    StreamProbe(expected_length)

        stream = self.create_stream()

        with self.assertRaisesRegex(ProbeError, "InvalidArgument"):
            stream.read_chunk(0)

        with self.assertRaises(OverflowError):
            stream.read_chunk(-1)

        with self.assertRaisesRegex(ProbeError, "InvalidArgument"):
            stream.reserve().write(b"x" * (CHUNK_BYTES + 1))

        with self.assertRaises(ProducerFailure) as failed:
            produce(stream, ["text"])  # type: ignore[list-item]

        self.assertIsInstance(failed.exception.__cause__, TypeError)
        self.assertEqual(stream.allocations, (0, 0))

    def test_empty_iterator_yields_to_close_and_finalization_keeps_primary_error(
        self,
    ) -> None:
        stream = self.create_stream()
        entered, finalized = threading.Event(), threading.Event()

        def source() -> Iterator[bytes]:
            try:
                while True:
                    entered.set()
                    yield b""
            finally:
                finalized.set()

        producing = self.run_in_background(
            lambda: produce(stream, source()), stream.close
        )

        self.assertTrue(entered.wait(5))
        stream.close()

        with self.assertRaises(ProducerFailure) as interrupted:
            producing()

        self.assertIsInstance(interrupted.exception.__cause__, ProbeError)
        self.assertTrue(finalized.is_set())
        self.assertEqual(stream.allocations, (0, 0))

        cause = ValueError("original producer error")
        cleanup = RuntimeError("iterator finalization error")

        class FailingIterator:
            def __iter__(self) -> FailingIterator:
                return self

            def __next__(self) -> bytes:
                raise cause

            def close(self) -> None:
                raise cleanup

        with self.assertRaises(ProducerFailure) as failed:
            produce(self.create_stream(), FailingIterator())

        self.assertIs(failed.exception.__cause__, cause)
        self.assertIs(failed.exception.__context__, cleanup)
