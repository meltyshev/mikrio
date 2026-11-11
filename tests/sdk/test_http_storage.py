"""boto3 against the local object service and native seed."""

import base64
import contextlib
import hashlib
import urllib.error
import urllib.request
import zlib
from email.header import decode_header

from botocore.exceptions import ClientError
from test_http_integrity import S3TestCase


class StorageTests(S3TestCase):
    def test_native_seed_and_default_checksum_persist_through_get_and_head(
        self,
    ) -> None:
        seed = self.client.get_object(
            Bucket="photos", Key="native-seed", ChecksumMode="ENABLED"
        )

        with contextlib.closing(seed["Body"]) as body:
            self.assertEqual(body.read(), b"native-to-sdk")

        self.assertEqual(seed["Metadata"], {"title": "=?UTF-8?B?Y2Fmw6k=?="})
        encoded, encoding = decode_header(seed["Metadata"]["title"])[0]
        self.assertIsInstance(encoded, bytes)
        self.assertEqual(encoded.decode(encoding), "café")
        self.assertEqual(seed["ContentType"], "text/plain")

        payload = bytes(index % 251 for index in range(257 * 1024))
        expected = base64.b64encode(zlib.crc32(payload).to_bytes(4, "big")).decode()

        put = self.client.put_object(
            Bucket="photos",
            Key="boto-shared",
            Body=payload,
            Metadata={"producer": "boto3"},
            ContentType="application/octet-stream",
        )

        self.assertEqual(put["ChecksumCRC32"], expected)
        self.assertEqual(put["ETag"], f'"{hashlib.md5(payload).hexdigest()}"')

        response = self.client.get_object(
            Bucket="photos", Key="boto-shared", ChecksumMode="ENABLED"
        )

        with contextlib.closing(response["Body"]) as body:
            self.assertEqual(body.read(), payload)

        head = self.client.head_object(
            Bucket="photos", Key="boto-shared", ChecksumMode="ENABLED"
        )

        for result in [response, head]:
            self.assertEqual(result["ChecksumCRC32"], expected)
            self.assertEqual(result["ContentLength"], len(payload))
            self.assertEqual(result["Metadata"], {"producer": "boto3"})
            self.assertEqual(result["ETag"], put["ETag"])

    def test_conditions_integrity_and_delete_preserve_authoritative_state(self) -> None:
        target = {"Bucket": "photos", "Key": "boto-condition"}
        put = self.client.put_object(**target, Body=b"original", IfNoneMatch="*")

        for fields, code in [
            ({"IfNoneMatch": "*"}, "PreconditionFailed"),
            ({"ChecksumCRC32": "AAAAAA=="}, "BadDigest"),
        ]:
            with self.assertRaises(ClientError) as raised:
                self.client.put_object(**target, Body=b"replacement", **fields)

            self.assertEqual(raised.exception.response["Error"]["Code"], code)

        response = self.client.get_object(**target)

        with contextlib.closing(response["Body"]) as body:
            self.assertEqual(body.read(), b"original")

        with self.assertRaises(ClientError) as raised:
            self.client.head_object(**target, IfNoneMatch=put["ETag"])

        self.assertEqual(
            raised.exception.response["ResponseMetadata"]["HTTPStatusCode"], 304
        )

        with self.assertRaises(ClientError) as raised:
            self.client.delete_object(**target, IfMatch='"wrong"')

        self.assertEqual(
            raised.exception.response["Error"]["Code"], "PreconditionFailed"
        )

        self.client.delete_object(**target, IfMatch=put["ETag"])

        with self.assertRaises(ClientError) as raised:
            self.client.get_object(**target)

        self.assertEqual(raised.exception.response["Error"]["Code"], "NoSuchKey")

    def test_presigned_read_and_range_preserve_exact_bytes(self) -> None:
        target = {"Bucket": "photos", "Key": "native-seed"}

        url = self.client.generate_presigned_url(
            "get_object", Params=target, ExpiresIn=60
        )

        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        with opener.open(url, timeout=5) as response:
            self.assertEqual(response.read(), b"native-to-sdk")

        altered = url.replace("/photos/native-seed?", "/photos/changed?")

        self.assertNotEqual(
            altered, url, "presigned URL must contain the original target path"
        )

        with self.assertRaises(urllib.error.HTTPError) as denied:
            opener.open(altered, timeout=5)

        with denied.exception as response:
            self.assertEqual(response.code, 403)

        selected = self.client.get_object(**target, Range="bytes=0-1")

        with contextlib.closing(selected["Body"]) as body:
            self.assertEqual(selected["ResponseMetadata"]["HTTPStatusCode"], 206)
            self.assertEqual(selected["ContentLength"], 2)
            self.assertEqual(selected["ContentRange"], "bytes 0-1/13")
            self.assertEqual(body.read(), b"na")
