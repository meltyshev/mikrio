"""Managed transfers over real HTTP with independent authorized readback."""

from __future__ import annotations

import contextlib
import io
import json
import os
import tempfile
from pathlib import Path
from typing import Any

from boto3.s3.transfer import TransferConfig
from botocore.exceptions import ClientError
from s3transfer.exceptions import CancelledError
from s3transfer.futures import NonThreadedExecutor
from s3transfer.manager import TransferManager
from s3transfer.subscribers import BaseSubscriber
from test_http_integrity import S3TestCase

PART_BYTES = 5 * 1024 * 1024


class NonSeekable:
    def __init__(self, body: bytes) -> None:
        self.body = io.BytesIO(body)

    def read(self, amount: int = -1) -> bytes:
        return self.body.read(amount)


class ProducerFailure(OSError):
    """The owned producer fails after its first part is acknowledged."""


class FailingSource(NonSeekable):
    def read(self, amount: int = -1) -> bytes:
        if self.body.tell() >= PART_BYTES:
            raise ProducerFailure("owned source failed")

        return super().read(amount)


class CancelAfterPart(BaseSubscriber):  # type: ignore[misc]
    def __init__(self, client: Any) -> None:
        self.client = client

    def on_queued(self, future: Any, **_kwargs: Any) -> None:
        self.client.meta.events.register(
            "after-call.s3.UploadPart", lambda **_kwargs: future.cancel()
        )


def config() -> TransferConfig:
    return TransferConfig(
        multipart_threshold=PART_BYTES,
        multipart_chunksize=PART_BYTES,
        max_concurrency=1,
        use_threads=False,
        preferred_transfer_client="classic",
    )


class TransferTests(S3TestCase):
    def setUp(self) -> None:
        super().setUp()
        self.peer = self.client_for(os.environ["mikrio_fixture_peer"])
        self.calls: list[str] = []
        self.events: list[dict[str, Any]] = []
        self.client.meta.events.register("before-call.s3", self.observe)
        self.client.meta.events.register("after-call.s3", self.settled)
        self.target = {"Bucket": "photos", "Key": "boto-transfer"}
        self.addCleanup(self.peer.delete_object, **self.target)
        self.addCleanup(self.retain)

        self.client.put_object(
            Bucket="photos",
            Key="boto-shared",
            Body=bytes(index % 251 for index in range(257 * 1024)),
        )

        self.calls.clear()
        self.events.clear()

    def observe(self, model: Any, **_kwargs: Any) -> None:
        self.calls.append(model.name)
        self.events.append({"operation": model.name, "stage": "issued"})

    def settled(self, model: Any, http_response: Any, **_kwargs: Any) -> None:
        self.events.append(
            {
                "operation": model.name,
                "stage": "response",
                "status": http_response.status_code,
            }
        )

    def retain(self) -> None:
        destination = os.environ.get("mikrio_sdk_live_record")

        if destination:
            path = Path(destination) / (self._testMethodName + ".json")

            path.write_text(
                json.dumps(
                    {
                        "origin": "live HTTP client calls",
                        "calls": self.calls,
                        "events": self.events,
                    },
                    indent=2,
                )
                + "\n"
            )

    def source_case(self, source_form: str, payload_bytes: int) -> None:
        content = b"x" * (payload_bytes - 1) + b"z"

        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source"
            source.write_bytes(content)

            if source_form == "filename":
                self.client.upload_file(
                    str(source), "photos", "boto-transfer", Config=config()
                )
            else:
                body = (
                    io.BytesIO(content)
                    if source_form == "seekable"
                    else NonSeekable(content)
                )

                self.client.upload_fileobj(
                    body, "photos", "boto-transfer", Config=config()
                )

            expected = (
                ["PutObject"]
                if payload_bytes < PART_BYTES
                else [
                    "CreateMultipartUpload",
                    *["UploadPart"] * (1 if payload_bytes == PART_BYTES else 2),
                    "CompleteMultipartUpload",
                ]
            )

            self.assertEqual(self.calls, expected)
            response = self.peer.get_object(**self.target, ChecksumMode="ENABLED")

            with contextlib.closing(response["Body"]) as reader:
                self.assertEqual(reader.read(), content)

            self.assertIn("ChecksumCRC32", response)
            destination = Path(directory) / "download"

            self.peer.download_file(
                "photos", "boto-transfer", str(destination), Config=config()
            )

            self.assertEqual(destination.read_bytes(), content)
            streamed = io.BytesIO()

            self.peer.download_fileobj(
                "photos", "boto-transfer", streamed, Config=config()
            )

            self.assertEqual(streamed.getvalue(), content)

        self.assertEqual(
            self.peer.list_multipart_uploads(Bucket="photos").get("Uploads", []), []
        )

    def assert_preserved(self) -> None:
        response = self.peer.get_object(**self.target)

        with contextlib.closing(response["Body"]) as body:
            self.assertEqual(body.read(), b"predecessor")

        self.assertEqual(
            self.peer.list_multipart_uploads(Bucket="photos").get("Uploads", []), []
        )

    def test_failed_managed_download_preserves_existing_file_and_fileobj(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "download"
            destination.write_bytes(b"local predecessor")
            stream = io.BytesIO(b"stream predecessor")

            for source in ("filename", "fileobj"):
                with self.assertRaises(ClientError) as error:
                    if source == "filename":
                        self.client.download_file(
                            "photos", "boto-missing", str(destination), Config=config()
                        )
                    else:
                        self.client.download_fileobj(
                            "photos", "boto-missing", stream, Config=config()
                        )

                self.assertEqual(
                    error.exception.response["ResponseMetadata"]["HTTPStatusCode"], 404
                )

                self.assertEqual(destination.read_bytes(), b"local predecessor")
                self.assertEqual(stream.getvalue(), b"stream predecessor")

        self.assertEqual(self.calls, ["HeadObject", "HeadObject"])

    def test_producer_failure_aborts_real_parts_and_preserves_destination(self) -> None:
        self.peer.put_object(**self.target, Body=b"predecessor")

        with self.assertRaisesRegex(ProducerFailure, "owned source failed"):
            self.client.upload_fileobj(
                FailingSource(b"x" * PART_BYTES),
                "photos",
                "boto-transfer",
                Config=config(),
            )

        self.assertEqual(
            self.calls, ["CreateMultipartUpload", "UploadPart", "AbortMultipartUpload"]
        )

        self.assert_preserved()

    def test_cancellation_after_acknowledged_part_aborts_and_preserves_destination(
        self,
    ) -> None:
        self.peer.put_object(**self.target, Body=b"predecessor")

        with TransferManager(
            self.client, config(), executor_cls=NonThreadedExecutor
        ) as manager:
            future = manager.upload(
                io.BytesIO(b"x" * (PART_BYTES + 1)),
                "photos",
                "boto-transfer",
                subscribers=[CancelAfterPart(self.client)],
            )

            with self.assertRaises(CancelledError):
                future.result()

        self.assertEqual(
            self.calls, ["CreateMultipartUpload", "UploadPart", "AbortMultipartUpload"]
        )

        self.assert_preserved()

    def test_retired_upload_fails_next_part_and_preserves_destination(self) -> None:
        self.peer.put_object(**self.target, Body=b"predecessor")

        def retire(http_response: Any, **_kwargs: Any) -> None:
            if http_response.status_code != 200:
                return

            uploads = self.peer.list_multipart_uploads(Bucket="photos")["Uploads"]
            self.assertEqual(len(uploads), 1)

            self.peer.abort_multipart_upload(
                **self.target, UploadId=uploads[0]["UploadId"]
            )

        self.client.meta.events.register("after-call.s3.UploadPart", retire)

        with self.assertRaises(ClientError) as error:
            self.client.upload_fileobj(
                io.BytesIO(b"x" * (PART_BYTES + 1)),
                "photos",
                "boto-transfer",
                Config=config(),
            )

        self.assertEqual(error.exception.response["Error"]["Code"], "NoSuchUpload")

        self.assertEqual(
            self.calls,
            [
                "CreateMultipartUpload",
                "UploadPart",
                "UploadPart",
                "AbortMultipartUpload",
            ],
        )

        self.assert_preserved()


def register_source_case(source_form: str, payload_bytes: int, boundary: str) -> None:
    def test(self: TransferTests) -> None:
        self.source_case(source_form, payload_bytes)

    test.__name__ = f"test_{source_form}_{boundary}_threshold_upload_and_download"
    setattr(TransferTests, test.__name__, test)


for source_form in ("filename", "seekable", "nonseekable"):
    for delta, boundary in ((-1, "below"), (0, "at"), (1, "above")):
        register_source_case(source_form, PART_BYTES + delta, boundary)
