"""Managed helper source/settlement observations with authored transport replies."""

from __future__ import annotations

import io
import json
import os
import tempfile
import threading
import unittest
from collections.abc import Callable
from concurrent.futures import CancelledError
from pathlib import Path
from typing import Any

from boto3.s3.transfer import TransferConfig
from s3transfer.futures import NonThreadedExecutor
from s3transfer.manager import TransferManager
from s3transfer.subscribers import BaseSubscriber
from test_request_defaults import Transport, client_for

PART_BYTES = 5 * 1024 * 1024


class NonSeekable:
    def __init__(self, body: bytes) -> None:
        self.body = io.BytesIO(body)

    def read(self, amount: int = -1) -> bytes:
        return self.body.read(amount)


class ProducerFailure(Exception):
    pass


class FailingSource(NonSeekable):
    def read(self, amount: int = -1) -> bytes:
        if self.body.tell() == PART_BYTES:
            raise ProducerFailure("owned source failed after its first part")

        return super().read(amount)


class PartTransport(Transport):
    def __init__(self, on_part: Callable[[], None] = lambda: None) -> None:
        super().__init__()
        self.on_part = on_part

    def send(self, request: Any) -> Any:
        response = super().send(request)

        if self.operation == "upload_part":
            self.on_part()

        return response


class CancelAfterPart(BaseSubscriber):  # type: ignore[misc]
    def __init__(self, transport: PartTransport) -> None:
        self.transport = transport

    def on_queued(self, future: Any, **_kwargs: Any) -> None:
        self.transport.on_part = future.cancel


def config() -> TransferConfig:
    return TransferConfig(
        multipart_threshold=PART_BYTES,
        multipart_chunksize=PART_BYTES,
        max_concurrency=1,
        use_threads=False,
        preferred_transfer_client="classic",
    )


def retain(name: str, cases: list[dict[str, Any]]) -> None:
    destination = os.environ.get("mikrio_sdk_defaults_record")

    if destination:
        Path(destination).with_name(f"python-transfer-{name}.json").write_text(
            json.dumps(
                {
                    "origin": "boto3/s3transfer; authored responses; no service execution",
                    "cases": cases,
                },
                indent=2,
            )
            + "\n"
        )


class TransferSourcesTests(unittest.TestCase):
    def test_parallel_parts_are_bounded_and_completion_waits_for_all_parts(
        self,
    ) -> None:
        entered = threading.Event()
        release = threading.Event()
        lock = threading.Lock()
        active = 0
        maximum = 0

        def hold_part() -> None:
            nonlocal active, maximum

            with lock:
                active += 1
                maximum = max(maximum, active)

                if active == 2:
                    entered.set()

            try:
                if not release.wait(5):
                    raise TimeoutError("parallel parts did not receive fixture release")
            finally:
                with lock:
                    active -= 1

        transport = PartTransport(hold_part)
        client = client_for(transport)
        self.addCleanup(client.close)

        parallel = TransferConfig(
            multipart_threshold=PART_BYTES,
            multipart_chunksize=PART_BYTES,
            max_concurrency=2,
            preferred_transfer_client="classic",
        )

        with TransferManager(client, parallel) as manager:
            future = manager.upload(
                io.BytesIO(b"x" * (2 * PART_BYTES + 1)), "bucket", "key"
            )

            try:
                self.assertTrue(entered.wait(5), "two real SDK part workers must enter")
                self.assertFalse(future.done())

                self.assertEqual(
                    [exchange["operation"] for exchange in transport.exchanges],
                    ["create_multipart_upload", "upload_part", "upload_part"],
                )
            finally:
                release.set()

            future.result()

        self.assertEqual(maximum, 2)

        self.assertEqual(
            [exchange["operation"] for exchange in transport.exchanges],
            [
                "create_multipart_upload",
                "upload_part",
                "upload_part",
                "upload_part",
                "complete_multipart_upload",
            ],
        )

        retain(
            "concurrency",
            [{"maximum_active_parts": maximum, "exchanges": transport.exchanges}],
        )

    def test_threshold_edges_for_file_seekable_and_nonseekable_sources(self) -> None:
        cases = []

        for source_form in ("filename", "seekable", "nonseekable"):
            for payload_bytes in (PART_BYTES - 1, PART_BYTES, PART_BYTES + 1):
                with self.subTest(source=source_form, size=payload_bytes):
                    transport = Transport()
                    client = client_for(transport)
                    self.addCleanup(client.close)
                    body = b"x" * payload_bytes

                    if source_form == "filename":
                        with tempfile.TemporaryDirectory() as directory:
                            path = Path(directory) / "source"
                            path.write_bytes(body)

                            client.upload_file(
                                str(path), "bucket", "key", Config=config()
                            )
                    else:
                        source = (
                            io.BytesIO(body)
                            if source_form == "seekable"
                            else NonSeekable(body)
                        )

                        client.upload_fileobj(source, "bucket", "key", Config=config())

                    expected = (
                        ["put_object"]
                        if payload_bytes < PART_BYTES
                        else [
                            "create_multipart_upload",
                            *(
                                ["upload_part"]
                                * (1 if payload_bytes == PART_BYTES else 2)
                            ),
                            "complete_multipart_upload",
                        ]
                    )

                    self.assertEqual(
                        [exchange["operation"] for exchange in transport.exchanges],
                        expected,
                    )

                    payloads = [
                        exchange
                        for exchange in transport.exchanges
                        if exchange["operation"] in {"put_object", "upload_part"}
                    ]

                    self.assertEqual(
                        sum(exchange["body_bytes"] for exchange in payloads),
                        payload_bytes,
                    )

                    self.assertTrue(
                        all(
                            "x-amz-checksum-crc32" in exchange["headers"]
                            for exchange in payloads
                        )
                    )

                    cases.append(
                        {
                            "source": source_form,
                            "size": payload_bytes,
                            "exchanges": transport.exchanges,
                        }
                    )

        retain("sources", cases)

    def test_late_producer_failure_aborts_without_completion(self) -> None:
        transport = Transport()
        client = client_for(transport)
        self.addCleanup(client.close)

        with self.assertRaises(ProducerFailure):
            client.upload_fileobj(
                FailingSource(b"x" * PART_BYTES), "bucket", "key", Config=config()
            )

        self.assertEqual(
            [exchange["operation"] for exchange in transport.exchanges],
            ["create_multipart_upload", "upload_part", "abort_multipart_upload"],
        )

        retain("failure", [{"source": "nonseekable", "exchanges": transport.exchanges}])

    def test_future_cancellation_after_a_part_aborts_and_does_not_complete(
        self,
    ) -> None:
        transport = PartTransport()
        client = client_for(transport)
        self.addCleanup(client.close)

        with TransferManager(
            client, config(), executor_cls=NonThreadedExecutor
        ) as manager:
            future = manager.upload(
                io.BytesIO(b"x" * (PART_BYTES + 1)),
                "bucket",
                "key",
                subscribers=[CancelAfterPart(transport)],
            )

            with self.assertRaises(CancelledError):
                future.result()

        self.assertEqual(
            [exchange["operation"] for exchange in transport.exchanges],
            ["create_multipart_upload", "upload_part", "abort_multipart_upload"],
        )

        retain("cancellation", [{"exchanges": transport.exchanges}])


if __name__ == "__main__":
    unittest.main()
