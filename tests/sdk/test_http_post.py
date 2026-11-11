"""Pinned boto3 forms publish through an ordinary owned public listener."""

import base64
import contextlib
import hashlib
import http.client
import os
import ssl
import zlib
from typing import Any
from urllib.parse import urlsplit

from botocore.exceptions import ClientError
from http_loss import lost_response
from test_http_integrity import S3TestCase


def upload(
    form: dict[str, Any], payload: bytes, *, chunked: bool = True
) -> tuple[int, bytes]:
    target = urlsplit(form["url"])

    if (
        target.scheme not in {"http", "https"}
        or target.hostname != "127.0.0.1"
        or target.port is None
        or not 1 <= target.port <= 65535
        or target.username is not None
        or target.password is not None
        or target.query
        or target.fragment
    ):
        raise ValueError("owned loopback fixture required")

    boundary = "mikrio-owned-sdk-form-boundary"

    preamble = b"".join(
        (
            f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n'
        ).encode()
        for name, value in form["fields"].items()
    )

    preamble += (
        f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="ignored.txt"\r\nContent-Type: ignored/file-header\r\n\r\n'
    ).encode()

    chunks = [
        preamble,
        *[payload[index : index + 4096] for index in range(0, len(payload), 4096)],
        f"\r\n--{boundary}--\r\n".encode(),
    ]

    connection = (
        http.client.HTTPSConnection(
            target.hostname,
            target.port,
            timeout=10,
            context=ssl.create_default_context(
                cafile=os.environ["mikrio_fixture_tls_ca"]
            ),
        )
        if target.scheme == "https"
        else http.client.HTTPConnection(target.hostname, target.port, timeout=10)
    )

    try:
        connection.request(
            "POST",
            target.path,
            body=iter(chunks) if chunked else b"".join(chunks),
            headers={"Content-Type": "multipart/form-data; boundary=" + boundary},
            encode_chunked=chunked,
        )

        response = connection.getresponse()
        return response.status, response.read()
    finally:
        connection.close()


class PostTests(S3TestCase):
    def test_lost_post_success_commits_and_explicit_form_replay_overwrites_peer(
        self,
    ) -> None:
        target = {"Bucket": "photos", "Key": "boto-lost-post"}
        self.addCleanup(self.client.delete_object, **target)
        payload = b"form A bytes"
        checksum = base64.b64encode(hashlib.sha256(payload).digest()).decode()
        tagging = "<Tagging><TagSet><Tag><Key>version</Key><Value>A</Value></Tag></TagSet></Tagging>"

        fields = {
            "tagging": tagging,
            "x-amz-checksum-algorithm": "SHA256",
            "x-amz-checksum-sha256": checksum,
            "success_action_status": "204",
        }

        form = self.client.generate_presigned_post(
            "photos",
            target["Key"],
            Fields=fields,
            Conditions=[
                *[{name: value} for name, value in fields.items()],
                ["content-length-range", len(payload), len(payload)],
            ],
            ExpiresIn=60,
        )

        self.client.put_object(
            **target, Body=b"previous bytes", Tagging="version=previous"
        )

        def inspect(expected: bytes, tag: str) -> None:
            selected = self.client.get_object(**target, ChecksumMode="ENABLED")

            with contextlib.closing(selected["Body"]) as body:
                self.assertEqual(body.read(), expected)

            self.assertEqual(
                self.client.get_object_tagging(**target)["TagSet"],
                [{"Key": "version", "Value": tag}],
            )

        with lost_response("POST", "/photos") as (endpoint, proxy):
            routed = {**form, "url": endpoint + "/photos"}

            with self.assertRaises(http.client.RemoteDisconnected):
                upload(routed, payload, chunked=False)

            self.assertEqual(proxy.attempts, 1)
            self.assertEqual(len(proxy.observations), 1)
            self.assertEqual(proxy.observations[0]["status"], 204)

        inspect(payload, "A")
        self.client.put_object(**target, Body=b"peer B bytes", Tagging="version=B")
        inspect(b"peer B bytes", "B")
        self.assertEqual(upload(form, payload)[0], 204)
        inspect(payload, "A")

    def test_generated_forms_checksums_fields_conditions_replay_and_durable_readback(
        self,
    ) -> None:
        payload = bytes(index % 251 for index in range(257 * 1024))
        encoded = base64.b64encode(zlib.crc32(payload).to_bytes(4, "big")).decode()

        form = self.client.generate_presigned_post(
            "photos",
            "boto-shared",
            Fields={
                "x-amz-checksum-algorithm": "CRC32",
                "x-amz-checksum-crc32": encoded,
            },
            Conditions=[
                {"x-amz-checksum-algorithm": "CRC32"},
                {"x-amz-checksum-crc32": encoded},
                ["content-length-range", len(payload), len(payload)],
            ],
            ExpiresIn=60,
        )

        self.assertEqual(upload(form, payload)[0], 204)
        target = {"Bucket": "photos", "Key": "boto-post /%+é"}
        self.addCleanup(self.client.delete_object, **target)

        for status in ["200", "201", "204"]:
            fields = {
                "Content-Type": "text/plain",
                "x-amz-meta-origin": "form",
                "success_action_status": status,
            }

            form = self.client.generate_presigned_post(
                "photos",
                target["Key"],
                Fields=fields,
                Conditions=[
                    *[{name: value} for name, value in fields.items()],
                    ["content-length-range", 0, 9],
                ],
                ExpiresIn=60,
            )

            for payload in [b"", b"original", b"replayed"]:
                response_status, body = upload(form, payload)
                self.assertEqual(response_status, int(status))
                self.assertEqual(b"<PostResponse" in body, status == "201")
                selected = self.client.get_object(**target)

                with contextlib.closing(selected["Body"]) as stream:
                    self.assertEqual(stream.read(), payload)

                self.assertEqual(selected["ContentType"], "text/plain")
                self.assertEqual(selected["Metadata"], {"origin": "form"})

            self.assertIn(b"EntityTooLarge", upload(form, b"too-long--")[1])

        for algorithm, encoded in [
            ("CRC32", "y/Q5Jg=="),
            ("CRC32C", "4waSgw=="),
            ("SHA1", "98O8HYCOBHMq32eZZczDTKeuNEE="),
            ("SHA256", "FeKw08M4keuw8e9gnsQZQgwg4yDOlMZfvIwzEkSOsiU="),
        ]:
            fields = {
                "x-amz-checksum-algorithm": algorithm,
                "x-amz-checksum-" + algorithm.lower(): encoded,
                "Content-MD5": base64.b64encode(
                    hashlib.md5(b"123456789").digest()
                ).decode(),
            }

            form = self.client.generate_presigned_post(
                "photos",
                target["Key"],
                Fields=fields,
                Conditions=[{name: value} for name, value in fields.items()],
                ExpiresIn=60,
            )

            self.assertEqual(upload(form, b"123456789")[0], 204)
            self.assertIn(b"BadDigest", upload(form, b"123456780")[1])

        # Conditions do not create fields. Missing the exact required form field fails.
        incomplete = self.client.generate_presigned_post(
            "photos",
            target["Key"],
            Conditions=[{"Content-Type": "text/plain"}],
            ExpiresIn=60,
        )

        rejected_status, rejected_body = upload(incomplete, b"invalid")
        self.assertEqual(rejected_status, 400)
        self.assertIn(b"InvalidPolicyDocument", rejected_body)
        selected = self.client.get_object(**target)

        with contextlib.closing(selected["Body"]) as stream:
            self.assertEqual(stream.read(), b"123456789")

    def test_lifecycle_ordinary_sdk_checksum_round_trip_and_delete(self) -> None:
        rules = [
            {
                "ID": "sdk-😀",
                "Status": "Enabled",
                "Filter": {"Prefix": "boto-expire/"},
                "Expiration": {"Days": 7},
                "AbortIncompleteMultipartUpload": {"DaysAfterInitiation": 1},
            },
            {"Status": "Disabled", "Filter": {}, "Expiration": {"Days": 3}},
        ]

        self.client.put_bucket_lifecycle_configuration(
            Bucket="photos", LifecycleConfiguration={"Rules": rules}
        )

        result = self.client.get_bucket_lifecycle_configuration(Bucket="photos")[
            "Rules"
        ]

        self.assertEqual(result[0], rules[0])
        self.assertTrue(result[1]["ID"])

        self.client.put_bucket_lifecycle_configuration(
            Bucket="photos", LifecycleConfiguration={"Rules": result}
        )

        self.assertEqual(
            self.client.get_bucket_lifecycle_configuration(Bucket="photos")["Rules"],
            result,
        )

        for _ in range(2):
            self.client.delete_bucket_lifecycle(Bucket="photos")

        with self.assertRaises(ClientError) as missing:
            self.client.get_bucket_lifecycle_configuration(Bucket="photos")

        self.assertEqual(
            missing.exception.response["Error"]["Code"], "NoSuchLifecycleConfiguration"
        )
