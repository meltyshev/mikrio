"""One lazy host delivery thread, holding only weak native observation handles."""

from __future__ import annotations

import logging
import threading
from collections import deque
from typing import Protocol


class Receiver(Protocol):
    def same_queue(self, other: Receiver) -> bool: ...
    def is_closed(self) -> bool: ...
    def take(self) -> str | None: ...
    def observer_failed(self) -> None: ...


_lock = threading.Lock()
_receivers: deque[Receiver] = deque()
_worker: threading.Thread | None = None


def start(receiver: Receiver) -> None:
    """Register after native open, never on module import or in an engine worker."""
    global _worker

    with _lock:
        live = [current for current in _receivers if not current.is_closed()]
        _receivers.clear()
        _receivers.extend(live)

        if not any(current.same_queue(receiver) for current in _receivers):
            _receivers.append(receiver)

        if _worker is not None:
            return

        logger = logging.getLogger("mikrio")

        worker = threading.Thread(
            target=_deliver, args=(logger,), name="mikrio-diagnostics", daemon=True
        )

        worker.start()
        _worker = worker


def _deliver(logger: logging.Logger) -> None:
    global _worker
    interval = threading.Event()

    try:
        while not interval.wait(0.25):
            with _lock:
                live = [receiver for receiver in _receivers if not receiver.is_closed()]
                _receivers.clear()
                _receivers.extend(live)

                if not _receivers:
                    _worker = None
                    return

                receiver = _receivers[0]
                _receivers.rotate(-1)

            message = receiver.take()

            if message is None:
                continue

            try:
                logger.warning("%s", message)
            except Exception:
                # Observer failures cannot affect storage or recursively expose
                # handler exception text. Host control-flow exceptions propagate.
                receiver.observer_failed()
    finally:
        # Host control-flow exceptions may terminate delivery, but cannot leave
        # an apparently live worker preventing later explicit opens restarting it.
        with _lock:
            if _worker is threading.current_thread():
                _worker = None
