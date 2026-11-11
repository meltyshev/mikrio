"""Bounded diagnostic delivery through a named Python logger."""

import logging
import threading

from _mikrio_diagnostics_probe import DiagnosticProbe


class LoggingDelivery:
    def __init__(self, probe: DiagnosticProbe) -> None:
        self.probe = probe
        self.logger = logging.getLogger("mikrio")
        self.closed = threading.Event()

        self.worker = threading.Thread(
            target=self._deliver, name="mikrio-diagnostic-probe", daemon=True
        )

        self.worker.start()

    def close(self) -> None:
        self.closed.set()
        self.probe.close()
        # Never join a host logger from required shutdown: its handler may be
        # blocked, including reentrantly calling close on this delivery worker.

    def _deliver(self) -> None:
        while not self.closed.wait(0.25):
            event = self.probe.take()

            if event is None:
                continue

            try:
                _, dropped, _, observer_failures = self.probe.stats

                self.logger.warning(
                    "storage work needs attention store_id=%s work=%s failure=%s occurrences=%d age_ms=%d dropped_observations=%d observer_failures=%d",
                    event.store_id,
                    event.work,
                    event.failure,
                    event.occurrences,
                    event.age_ms,
                    dropped,
                    observer_failures,
                )
            except Exception:
                # No recursive logging or exception repr: a handler may put
                # secrets in its own exception. Control-flow exceptions remain
                # the host's boundary, not storage errors or rollback evidence.
                self.probe.observer_failed()
