"""Controlled diagnostic delivery through the native diagnostic probe."""

from __future__ import annotations

import logging
import subprocess
import sys
import threading
import time
import unittest
from collections.abc import Callable
from pathlib import Path

from _mikrio_diagnostics_probe import DiagnosticProbe
from diagnostic_delivery import LoggingDelivery

SECRET = "secret-key signed-url private-path body-canary"


class CallbackHandler(logging.Handler):
    def __init__(self, callback: Callable[[logging.LogRecord], None]) -> None:
        super().__init__()
        self.callback = callback

    def emit(self, record: logging.LogRecord) -> None:
        self.callback(record)


class DiagnosticTests(unittest.TestCase):
    def setUp(self) -> None:
        self.logger = logging.getLogger("mikrio")
        self.handlers = self.logger.handlers[:]
        self.level = self.logger.level
        self.propagate = self.logger.propagate
        self.disabled = self.logger.disabled
        self.addCleanup(self.restore_logger)
        self.logger.handlers = []
        self.logger.setLevel(logging.WARNING)
        self.logger.propagate = False
        self.logger.disabled = False

    def restore_logger(self) -> None:
        self.logger.handlers = self.handlers
        self.logger.setLevel(self.level)
        self.logger.propagate = self.propagate
        self.logger.disabled = self.disabled

    def start_delivery(
        self, callback: Callable[[logging.LogRecord], None]
    ) -> tuple[DiagnosticProbe, LoggingDelivery]:
        handler = CallbackHandler(callback)
        self.addCleanup(handler.close)
        self.logger.addHandler(handler)
        probe = DiagnosticProbe()
        self.addCleanup(probe.close)
        delivery = LoggingDelivery(probe)
        self.addCleanup(self.close_delivery, delivery)
        return probe, delivery

    def close_delivery(self, delivery: LoggingDelivery) -> None:
        delivery.close()
        delivery.worker.join(5)

        self.assertFalse(
            delivery.worker.is_alive(), "owned diagnostic worker did not settle"
        )

    def wait_until(self, predicate: Callable[[], bool]) -> None:
        deadline = time.monotonic() + 5

        while not predicate():
            self.assertLess(
                time.monotonic(), deadline, "diagnostic handshake timed out"
            )

            time.sleep(0.005)

    def test_idle_named_logger_redacts_and_coalesces_repeated_failures(self) -> None:
        records: list[logging.LogRecord] = []
        probe, _ = self.start_delivery(records.append)

        for _ in range(10_000):
            probe.emit(SECRET)

        self.assertEqual(probe.stats[:3], (1, 0, 9999))
        self.wait_until(lambda: len(records) == 1)
        self.assertEqual(records[0].name, "mikrio")
        message = records[0].getMessage()
        self.assertIn("work=cleanup failure=permission occurrences=10000", message)

        for canary in SECRET.split():
            self.assertNotIn(canary, message)

        self.assertEqual(probe.stats[0], 0)

    def test_throwing_handler_is_contained_without_recursive_logging(self) -> None:
        calls = 0

        def output(record: logging.LogRecord) -> None:
            nonlocal calls
            calls += 1
            raise ValueError(SECRET)

        probe, delivery = self.start_delivery(output)
        probe.emit(SECRET)
        self.wait_until(lambda: probe.stats[3] == 1)
        probe.emit(SECRET)
        self.wait_until(lambda: probe.stats[3] == 2)
        delivery.close()
        self.assertEqual(calls, 2)
        self.assertEqual(probe.stats[0], 0)

    def test_disabled_logger_does_not_change_native_queue_or_close(self) -> None:
        records: list[logging.LogRecord] = []
        probe, delivery = self.start_delivery(records.append)
        self.logger.disabled = True
        probe.emit(SECRET)
        self.wait_until(lambda: probe.stats[0] == 0)
        delivery.close()
        self.assertEqual(records, [])
        self.assertEqual(probe.stats, (0, 0, 0, 0))

    def test_blocked_handler_retains_one_record_and_cannot_prevent_close(self) -> None:
        entered, release = threading.Event(), threading.Event()
        calls = 0

        def output(record: logging.LogRecord) -> None:
            nonlocal calls
            calls += 1
            entered.set()

            if not release.wait(5):
                raise TimeoutError("fixture observer was not released")

            raise ValueError(SECRET)  # A late observer failure after native close.

        probe, delivery = self.start_delivery(output)
        self.addCleanup(release.set)
        probe.emit(SECRET)
        self.assertTrue(entered.wait(5))

        for _ in range(10_000):
            probe.emit(SECRET)

        self.assertEqual(probe.stats[0], 1)
        delivery.close()
        self.assertFalse(release.is_set())
        self.assertEqual(probe.stats[0:2], (0, 10_000))
        probe.emit(SECRET)
        release.set()
        delivery.worker.join(5)
        self.assertFalse(delivery.worker.is_alive())
        self.assertEqual(calls, 1)
        self.assertEqual(probe.stats[1], 10_001)
        self.assertEqual(probe.stats[3], 1)

    def test_handler_can_reenter_and_close_without_joining_itself(self) -> None:
        closed = threading.Event()
        probe = DiagnosticProbe()
        self.addCleanup(probe.close)
        delivery: LoggingDelivery

        def output(record: logging.LogRecord) -> None:
            probe.emit(SECRET)
            delivery.close()
            closed.set()

        handler = CallbackHandler(output)
        self.addCleanup(handler.close)
        self.logger.addHandler(handler)
        delivery = LoggingDelivery(probe)
        self.addCleanup(self.close_delivery, delivery)
        probe.emit(SECRET)
        self.assertTrue(closed.wait(5))
        delivery.worker.join(5)
        self.assertFalse(delivery.worker.is_alive())
        self.assertEqual(probe.stats[0:2], (0, 1))

    def test_import_and_idle_delivery_preserve_host_globals_and_process_exit(
        self,
    ) -> None:
        script = """
import logging
import sys
import threading

before = (
    logging.root.handlers[:],
    threading.excepthook,
    sys.excepthook,
    len(threading.enumerate()),
)

# Import after the snapshot so initialization side effects are observable.
from _mikrio_diagnostics_probe import DiagnosticProbe  # noqa: E402
from diagnostic_delivery import LoggingDelivery  # noqa: E402

assert before == (
    logging.root.handlers[:],
    threading.excepthook,
    sys.excepthook,
    len(threading.enumerate()),
)

delivery = LoggingDelivery(DiagnosticProbe())
assert before[:3] == (logging.root.handlers[:], threading.excepthook, sys.excepthook)
print("ready", flush=True)
"""

        # The private adapter is a copied test module, never an installed Store
        # export. The extension still imports from the isolated consumer venv.
        completed = subprocess.run(
            [sys.executable, "-c", script],
            cwd=Path(__file__).parent,
            check=True,
            text=True,
            capture_output=True,
            timeout=5,
        )

        self.assertEqual(completed.stdout.strip(), "ready")
        self.assertEqual(completed.stderr, "")
