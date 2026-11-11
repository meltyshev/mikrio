"""Real boto3 requests to the owned Rust observation sink, not persistent S3."""

import base64
import contextlib
import hashlib
import json
import os
import ssl
import unittest
import urllib.error
import urllib.request
from typing import Any

import boto3
from botocore.config import Config
from botocore.exceptions import ClientError

BODY = b"123456789"


class S3TestCase(unittest.TestCase):
    def setUp(self) -> None:
        self.client = self.client_for(os.environ["mikrio_fixture_endpoint"])

    def client_for(self, endpoint: str, *, read_timeout: float = 5) -> Any:
        client = boto3.client(
            "s3",
            endpoint_url=endpoint,
            region_name="us-east-1",
            aws_access_key_id=os.environ.get(
                "mikrio_fixture_key", "mikrio_a00112233445566778899AABBCCDDEEFF"
            ),
            aws_secret_access_key=os.environ.get(
                "mikrio_fixture_secret", "synthetic-http-probe-secret"
            ),
            config=Config(
                signature_version="s3v4",
                retries={"max_attempts": 0},
                connect_timeout=3,
                read_timeout=read_timeout,
                proxies={},
                s3={"addressing_style": "path"},
            ),
        )

        self.addCleanup(client.close)
        return client


class IntegrityTests(S3TestCase):
    def test_metadata_entry_boundary_projects_without_exceeding_client_headers(
        self,
    ) -> None:
        metadata = {f"key{index:02}": "" for index in range(64)}
        result = self.client.get_object(Bucket="photos", Key="metadata-limit")

        with contextlib.closing(result["Body"]) as body:
            self.assertEqual(body.read(), b"metadata-limit")

        self.assertEqual(result["Metadata"], metadata)

        with self.assertRaises(ClientError) as raised:
            self.client.put_object(
                Bucket="photos",
                Key="metadata-over",
                Body=b"",
                Metadata={**metadata, "extra": ""},
            )

        self.assertEqual(raised.exception.response["Error"]["Code"], "InvalidArgument")

    def test_explicit_flexible_algorithms_preserve_values_and_reject_corruption(
        self,
    ) -> None:
        for algorithm, encoded in [
            ("CRC32", "y/Q5Jg=="),
            ("CRC32C", "4waSgw=="),
            ("CRC64NVME", "rosUhgp5mIg="),
            ("SHA1", "98O8HYCOBHMq32eZZczDTKeuNEE="),
            ("SHA256", "FeKw08M4keuw8e9gnsQZQgwg4yDOlMZfvIwzEkSOsiU="),
        ]:
            with self.subTest(algorithm=algorithm):
                fields = {"Checksum" + algorithm: encoded}

                result = self.client.put_object(
                    Bucket="photos",
                    Key="boto-explicit",
                    Body=BODY,
                    ChecksumAlgorithm=algorithm,
                    **fields,
                )

                self.assertEqual(result["Checksum" + algorithm], encoded)

                with self.assertRaises(ClientError) as raised:
                    self.client.put_object(
                        Bucket="photos",
                        Key="boto-corrupt",
                        Body=b"123456780",
                        ChecksumAlgorithm=algorithm,
                        **fields,
                    )

                self.assertEqual(
                    raised.exception.response["Error"]["Code"], "BadDigest"
                )

    def test_default_checksum_and_content_md5_are_verified(self) -> None:
        result = self.client.put_object(
            Bucket="photos",
            Key="boto-default",
            Body=BODY,
            ContentMD5=base64.b64encode(hashlib.md5(BODY).digest()).decode(),
        )

        self.assertEqual(result["ETag"], '"25f9e794323b453885f5181f1b624d0b"')
        self.assertEqual(result["ChecksumCRC32"], "y/Q5Jg==")

    def test_bad_crc32_and_md5_return_machine_visible_bad_digest(self) -> None:
        for fields in [
            {"ChecksumCRC32": "AAAAAA=="},
            {"ContentMD5": "1B2M2Y8AsgTpgAmY7PhCfg=="},
        ]:
            with self.subTest(fields=tuple(fields)):
                with self.assertRaises(ClientError) as raised:
                    self.client.put_object(
                        Bucket="photos", Key="boto-bad", Body=BODY, **fields
                    )

                self.assertEqual(
                    raised.exception.response["Error"]["Code"], "BadDigest"
                )

                self.assertEqual(
                    raised.exception.response["ResponseMetadata"]["HTTPStatusCode"], 400
                )

    def test_presign_binds_path_and_allows_the_original_request(self) -> None:
        url = self.client.generate_presigned_url(
            "put_object",
            Params={"Bucket": "photos", "Key": "boto-presign"},
            ExpiresIn=60,
        )

        opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPSHandler(
                context=ssl.create_default_context(
                    cafile=os.environ.get("mikrio_fixture_tls_ca"),
                )
            ),
        )

        with opener.open(
            urllib.request.Request(url, data=BODY, method="PUT"), timeout=5
        ) as response:
            self.assertEqual(response.status, 200)

        changed = url.replace("/photos/boto-presign?", "/photos/changed?")
        self.assertNotEqual(changed, url)

        with self.assertRaises(urllib.error.HTTPError) as raised:
            opener.open(
                urllib.request.Request(changed, data=BODY, method="PUT"), timeout=5
            )

        with raised.exception as response:
            self.assertEqual(response.code, 403)

    def test_sdk_post_policy_delivers_only_file_bytes(self) -> None:
        post = self.client.generate_presigned_post(
            "photos",
            "boto-post",
            Fields={"success_action_status": "204"},
            Conditions=[
                {"success_action_status": "204"},
                ["content-length-range", 9, 9],
            ],
            ExpiresIn=60,
        )

        boundary = "mikrio-boto-form"

        parts = [
            f'--{boundary}\r\nContent-Disposition: form-data; name="{key}"\r\n\r\n{value}\r\n'.encode()
            for key, value in post["fields"].items()
        ]

        parts.append(
            f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="sample.txt"\r\n\r\n'.encode()
            + BODY
        )

        parts.append(f"\r\n--{boundary}--\r\n".encode())

        request = urllib.request.Request(
            post["url"],
            data=b"".join(parts),
            headers={
                "Content-Type": f"multipart/form-data; boundary={boundary}",
            },
            method="POST",
        )

        opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPSHandler(
                context=ssl.create_default_context(
                    cafile=os.environ.get("mikrio_fixture_tls_ca"),
                )
            ),
        )

        with opener.open(request, timeout=5) as response:
            self.assertEqual(response.status, 204)
            self.assertEqual(response.read(), b"")


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(IntegrityTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)

    print(
        json.dumps(
            {
                "tests": result.testsRun,
                "failures": len(result.failures),
                "errors": len(result.errors),
            }
        )
    )

    raise SystemExit(0 if result.wasSuccessful() else 1)
