"""Observe SDK middleware at its transport boundary, without sockets.

Responses are authored protocol fixtures, not observations of AWS or mikrio.
Only selected non-authentication headers and payload hashes leave the transport.
"""

from __future__ import annotations

import hashlib
import io
import json
import os
import unittest
from collections.abc import Iterator
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

import boto3
from boto3.s3.transfer import TransferConfig
from botocore import xform_name
from botocore.awsrequest import AWSResponse
from botocore.config import Config

BODY = b"123456789"
HEADERS = {
    "content-md5",
    "content-type",
    "x-amz-checksum-algorithm",
    "x-amz-checksum-type",
    "x-amz-sdk-checksum-algorithm",
    "x-amz-checksum-crc32",
    "x-amz-checksum-crc32c",
    "x-amz-checksum-crc64nvme",
    "x-amz-checksum-sha1",
    "x-amz-checksum-sha256",
    "x-amz-copy-source",
    "x-amz-copy-source-range",
    "x-amz-metadata-directive",
    "x-amz-tagging-directive",
    "x-amz-object-attributes",
}
MAX_BODY_BYTES = 6 * 1024 * 1024


class ResponseBody:
    def __init__(self, body: bytes) -> None:
        self.body = body

    def stream(self, _amount: int = 1024, **_options: object) -> Iterator[bytes]:
        yield self.body


class Transport:
    def __init__(self) -> None:
        self.exchanges: list[dict[str, Any]] = []
        self.operation = ""
        self.workflow = "low-level"

    def select_operation(self, model: Any, **_context: Any) -> None:
        self.operation = xform_name(model.name)

    def send(self, request: Any) -> Any:
        parsed = urlsplit(request.url)

        if parsed.scheme != "http" or parsed.netloc != "127.0.0.1:1":
            raise AssertionError("SDK request escaped the owned transport")

        body = request.body

        if hasattr(body, "read"):
            body = body.read(MAX_BODY_BYTES + 1)

        body = body or b""

        if not isinstance(body, bytes) or len(body) > MAX_BODY_BYTES:
            raise AssertionError("unexpected SDK request body")

        if (
            self.operation == "complete_multipart_upload"
            and self.workflow == "managed-upload"
        ):
            if b"<ChecksumCRC32>y/Q5Jg==</ChecksumCRC32>" not in body:
                raise AssertionError(
                    "managed completion lost the acknowledged part checksum"
                )

        headers = {
            name.lower(): value.decode() if isinstance(value, bytes) else value
            for name, value in request.headers.items()
            if name.lower() in HEADERS
        }

        self.exchanges.append(
            {
                "operation": self.operation,
                "workflow": self.workflow,
                "method": request.method,
                "headers": headers,
                "body_bytes": len(body),
                "body_sha256": hashlib.sha256(body).hexdigest(),
            }
        )

        response = {
            "create_multipart_upload": "<InitiateMultipartUploadResult><Bucket>bucket</Bucket><Key>key</Key><UploadId>owned-upload</UploadId></InitiateMultipartUploadResult>",
            "copy_object": '<CopyObjectResult><ETag>"fixture"</ETag><LastModified>2026-10-01T00:00:00Z</LastModified></CopyObjectResult>',
            "upload_part_copy": '<CopyPartResult><ETag>"fixture"</ETag><LastModified>2026-10-01T00:00:00Z</LastModified></CopyPartResult>',
            "complete_multipart_upload": '<CompleteMultipartUploadResult><Bucket>bucket</Bucket><Key>key</Key><ETag>"fixture-1"</ETag></CompleteMultipartUploadResult>',
        }.get(self.operation, "")

        response_headers = {"content-type": "application/xml", "etag": '"fixture"'}

        if self.operation == "upload_part" and "x-amz-checksum-crc32" in headers:
            response_headers["x-amz-checksum-crc32"] = headers["x-amz-checksum-crc32"]

        return AWSResponse(
            request.url,
            200,
            response_headers,
            ResponseBody(response.encode()),
        )

    def close(self) -> None:
        pass


def client_for(transport: Transport) -> Any:
    client = boto3.client(
        "s3",
        endpoint_url="http://127.0.0.1:1",
        region_name="us-east-1",
        aws_access_key_id="synthetic-key",
        aws_secret_access_key="synthetic-secret",
        config=Config(
            signature_version="s3v4",
            retries={"max_attempts": 0},
            proxies={},
            s3={"addressing_style": "path"},
        ),
    )

    client._endpoint.http_session.close()
    client._endpoint.http_session = transport
    client.meta.events.register("before-call.s3", transport.select_operation)
    return client


class RequestDefaultsTests(unittest.TestCase):
    def test_default_and_explicit_checksum_requests_reach_the_transport(self) -> None:
        transport = Transport()
        client = client_for(transport)
        self.addCleanup(client.close)

        cases: list[tuple[str, dict[str, Any]]] = [
            ("put_object", {"Body": io.BytesIO(BODY)}),
            ("create_multipart_upload", {}),
            (
                "upload_part",
                {"UploadId": "owned-upload", "PartNumber": 1, "Body": io.BytesIO(BODY)},
            ),
            (
                "complete_multipart_upload",
                {
                    "UploadId": "owned-upload",
                    "MultipartUpload": {
                        "Parts": [{"PartNumber": 1, "ETag": '"fixture"'}]
                    },
                },
            ),
            ("copy_object", {"CopySource": {"Bucket": "source", "Key": "literal /+é"}}),
            (
                "upload_part_copy",
                {
                    "UploadId": "owned-upload",
                    "PartNumber": 1,
                    "CopySource": {"Bucket": "source", "Key": "literal /+é"},
                },
            ),
            (
                "create_multipart_upload",
                {"ChecksumAlgorithm": "SHA256", "ChecksumType": "COMPOSITE"},
            ),
        ]

        for operation, inputs in cases:
            getattr(client, operation)(Bucket="bucket", Key="key", **inputs)

        self.assertEqual(len(transport.exchanges), len(cases))

        for index in (1, 3, 4, 5):
            with self.subTest(request=index):
                self.assertFalse(
                    any(
                        "checksum" in name or name.endswith("directive")
                        for name in transport.exchanges[index]["headers"]
                    )
                )

        for index in (4, 5):
            self.assertEqual(
                transport.exchanges[index]["headers"]["x-amz-copy-source"],
                "source/literal%20/%2B%C3%A9",
            )

        transport.workflow = "managed-upload"

        client.upload_fileobj(
            io.BytesIO(BODY),
            "bucket",
            "managed",
            Config=TransferConfig(
                multipart_threshold=1,
                multipart_chunksize=5 * 1024 * 1024,
                max_concurrency=1,
                use_threads=False,
                preferred_transfer_client="classic",
            ),
        )

        self.assertEqual(
            [entry["operation"] for entry in transport.exchanges[7:]],
            ["create_multipart_upload", "upload_part", "complete_multipart_upload"],
        )

        self.assertEqual(
            transport.exchanges[7]["headers"]["x-amz-checksum-algorithm"], "CRC32"
        )

        self.assertEqual(
            transport.exchanges[8]["headers"]["x-amz-checksum-crc32"], "y/Q5Jg=="
        )

        record = {
            "origin": "boto3 controlled transport; authored responses; no service execution",
            "boto3": boto3.__version__,
            "source_form": "seekable BytesIO over explicit HTTP endpoint",
            "exchanges": transport.exchanges,
        }

        destination = os.environ.get("mikrio_sdk_defaults_record")

        if destination:
            Path(destination).write_text(json.dumps(record, indent=2) + "\n")

        self.assertEqual(
            transport.exchanges[0]["headers"]["x-amz-checksum-crc32"], "y/Q5Jg=="
        )

        self.assertEqual(
            transport.exchanges[2]["headers"]["x-amz-checksum-crc32"], "y/Q5Jg=="
        )

        self.assertEqual(
            transport.exchanges[6]["headers"]["x-amz-checksum-algorithm"], "SHA256"
        )

        self.assertEqual(
            transport.exchanges[6]["headers"]["x-amz-checksum-type"], "COMPOSITE"
        )


if __name__ == "__main__":
    unittest.main()
