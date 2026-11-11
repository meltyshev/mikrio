"""Small durable multipart workflows using ordinary pinned boto3 checksums."""

import contextlib
import hashlib
import io
import json
import os
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any
from urllib.parse import quote

from boto3.s3.transfer import TransferConfig
from botocore.exceptions import ClientError, ConnectionClosedError
from http_loss import lost_response
from s3transfer.exceptions import S3CopyFailedError
from test_http_integrity import S3TestCase


class MultipartTests(S3TestCase):
    def test_durable_copy_and_completion_outlast_client_idle_without_replay(
        self,
    ) -> None:
        source = {"Bucket": "photos", "Key": "boto-progress-source"}
        copied = {"Bucket": "photos", "Key": "boto-progress-copy"}
        completed = {"Bucket": "photos", "Key": "boto-progress-completion"}

        for target in (source, copied, completed):
            self.addCleanup(self.client.delete_object, **target)

        read_timeout = 0.5

        client = self.client_for(
            os.environ["mikrio_fixture_endpoint"], read_timeout=read_timeout
        )

        observations: list[dict[str, Any]] = []

        def observe(
            model: Any, http_response: Any, parsed: Any, **_kwargs: Any
        ) -> None:
            body = http_response.content
            self.assertIsInstance(body, bytes)
            declaration, separator, remainder = body.partition(b"?>")
            self.assertTrue(declaration.startswith(b"<?xml"))
            self.assertEqual(separator, b"?>")
            whitespace = len(remainder) - len(remainder.lstrip())
            self.assertGreaterEqual(whitespace, 2)
            self.assertNotIn(b"<Error>", body)
            self.assertEqual(http_response.status_code, 200)
            self.assertEqual(parsed["ResponseMetadata"]["RetryAttempts"], 0)

            observations.append(
                {"operation": model.name, "status": 200, "whitespace_bytes": whitespace}
            )

        client.meta.events.register("after-call.s3.CopyObject", observe)
        client.meta.events.register("after-call.s3.CompleteMultipartUpload", observe)
        expected = hashlib.sha256()
        expected_md5 = hashlib.md5()
        part_digests = []
        part_bytes = 16 * 1024 * 1024
        total_bytes = 16 * part_bytes

        def retain() -> None:
            destination = Path(os.environ["mikrio_sdk_live_record"])

            (destination / (self._testMethodName + ".json")).write_text(
                json.dumps(
                    {
                        "origin": "live durable HTTP work",
                        "transport": "TLS"
                        if os.environ["mikrio_fixture_endpoint"].startswith("https://")
                        else "plaintext",
                        "object_bytes": total_bytes,
                        "part_count": 16,
                        "read_timeout_seconds": read_timeout,
                        "results": observations,
                    },
                    indent=2,
                )
                + "\n"
            )

        self.addCleanup(retain)

        def verify(target: dict[str, str]) -> None:
            selected = self.client.get_object(**target, ChecksumMode="ENABLED")
            self.assertEqual(selected["ContentLength"], total_bytes)
            actual = hashlib.sha256()
            count = 0

            with contextlib.closing(selected["Body"]) as body:
                while chunk := body.read(64 * 1024):
                    actual.update(chunk)
                    count += len(chunk)

            self.assertEqual(count, total_bytes)
            self.assertEqual(actual.digest(), expected.digest())

            self.assertEqual(
                self.client.get_object_tagging(**target)["TagSet"],
                [{"Key": "kind", "Value": "progress"}],
            )

        with tempfile.TemporaryFile() as body:
            for index in range(16):
                chunk = bytes([index]) * part_bytes
                body.write(chunk)
                expected.update(chunk)
                expected_md5.update(chunk)
                part_digests.append(hashlib.md5(chunk).digest())

            body.seek(0)
            self.client.put_object(**source, Body=body, Tagging="kind=progress")

            started = time.monotonic()
            response = client.copy_object(**copied, CopySource=source)
            elapsed = time.monotonic() - started
            self.assertGreater(elapsed, read_timeout)
            observations[-1]["elapsed_seconds"] = elapsed

            self.assertEqual(
                response["CopyObjectResult"]["ETag"], f'"{expected_md5.hexdigest()}"'
            )

            verify(copied)

            created = self.client.create_multipart_upload(
                **completed,
                Tagging="kind=progress",
                ChecksumAlgorithm="CRC32",
                ChecksumType="COMPOSITE",
            )

            upload = {**completed, "UploadId": created["UploadId"]}

            def abort_pending() -> None:
                try:
                    self.client.abort_multipart_upload(**upload)
                except ClientError as error:
                    if error.response["Error"]["Code"] != "NoSuchUpload":
                        raise

            self.addCleanup(abort_pending)
            parts = []
            body.seek(0)

            for number in range(1, 17):
                part = self.client.upload_part(
                    **upload, PartNumber=number, Body=body.read(part_bytes)
                )

                parts.append(
                    {
                        "PartNumber": number,
                        "ETag": part["ETag"],
                        "ChecksumCRC32": part["ChecksumCRC32"],
                    }
                )

            started = time.monotonic()

            response = client.complete_multipart_upload(
                **upload, MultipartUpload={"Parts": parts}
            )

            elapsed = time.monotonic() - started
            self.assertGreater(elapsed, read_timeout)
            observations[-1]["elapsed_seconds"] = elapsed
            etag = hashlib.md5(b"".join(part_digests)).hexdigest()
            self.assertEqual(response["ETag"], f'"{etag}-16"')
            verify(completed)

        self.assertEqual(
            [row["operation"] for row in observations],
            ["CopyObject", "CompleteMultipartUpload"],
        )

    def test_lost_completion_success_is_visible_and_duplicate_is_no_such_upload(
        self,
    ) -> None:
        target = {"Bucket": "photos", "Key": "boto-lost-completion"}
        self.addCleanup(self.client.delete_object, **target)

        self.client.put_object(
            **target, Body=b"previous bytes", Tagging="version=previous"
        )

        created = self.client.create_multipart_upload(**target, Tagging="version=new")
        upload = {**target, "UploadId": created["UploadId"]}

        def abort_pending() -> None:
            try:
                self.client.abort_multipart_upload(**upload)
            except ClientError as error:
                if error.response["Error"]["Code"] != "NoSuchUpload":
                    raise

        self.addCleanup(abort_pending)
        part = self.client.upload_part(**upload, PartNumber=1, Body=b"abc")
        parts = {"Parts": [{"PartNumber": 1, "ETag": part["ETag"]}]}

        path = "/photos/boto-lost-completion?uploadId=" + quote(
            created["UploadId"], safe=""
        )

        with lost_response("POST", path) as (endpoint, proxy):
            client = self.client_for(endpoint)

            with self.assertRaises(ConnectionClosedError):
                client.complete_multipart_upload(**upload, MultipartUpload=parts)

            self.assertEqual(proxy.attempts, 1)
            self.assertEqual(len(proxy.observations), 1)
            self.assertEqual(proxy.observations[0]["status"], 200)

        response = self.client.get_object(**target, ChecksumMode="ENABLED")

        with contextlib.closing(response["Body"]) as body:
            self.assertEqual(body.read(), b"abc")

        self.assertEqual(response["ETag"], '"af5da9f45af7a300e3aded972f8ff687-1"')

        self.assertEqual(
            self.client.get_object_tagging(**target)["TagSet"],
            [{"Key": "version", "Value": "new"}],
        )

        with self.assertRaises(ClientError) as duplicate:
            self.client.complete_multipart_upload(**upload, MultipartUpload=parts)

        self.assertEqual(duplicate.exception.response["Error"]["Code"], "NoSuchUpload")

        self.assertEqual(
            duplicate.exception.response["ResponseMetadata"]["HTTPStatusCode"], 404
        )

        self.assertEqual(self.client.head_object(**target)["ETag"], response["ETag"])

        self.assertFalse(
            self.client.list_multipart_uploads(Bucket="photos").get("Uploads")
        )

    def test_managed_copy_rejects_changed_source_and_explicit_retry_reads_new_bytes(
        self,
    ) -> None:
        source = {"Bucket": "photos", "Key": "boto-changing-copy-source"}
        destination = {"Bucket": "photos", "Key": "boto-changing-copy-destination"}
        peer = self.client_for(os.environ["mikrio_fixture_endpoint"])
        original = b"a" * (5 * 1024 * 1024) + b"original tail"
        replacement = b"b" * (5 * 1024 * 1024) + b"replacement tail"

        for target in (source, destination):
            self.addCleanup(peer.delete_object, **target)

        selected = peer.put_object(**source, Body=original, Tagging="version=old")

        previous = peer.put_object(
            **destination,
            Body=b"previous destination",
            Metadata={"generation": "previous"},
            Tagging="version=previous",
        )

        config = TransferConfig(
            multipart_threshold=5 * 1024 * 1024,
            multipart_chunksize=5 * 1024 * 1024,
            max_concurrency=1,
            preferred_transfer_client="classic",
        )

        parts: list[int] = []
        aborted: list[int] = []

        def replace_source(params: Any, **_kwargs: Any) -> None:
            self.assertEqual(params["CopySourceIfMatch"], selected["ETag"])
            parts.append(params["PartNumber"])

            if params["PartNumber"] != 2:
                return

            # One request worker has completed part 1 before asking for part 2.
            peer.put_object(
                **source,
                Body=replacement,
                Metadata={"generation": "replacement"},
                Tagging="version=new",
            )

        def observe_abort(http_response: Any, **_kwargs: Any) -> None:
            aborted.append(http_response.status_code)

        part_event = "before-parameter-build.s3.UploadPartCopy"
        abort_event = "after-call.s3.AbortMultipartUpload"
        self.client.meta.events.register(part_event, replace_source)
        self.client.meta.events.register(abort_event, observe_abort)

        try:
            with self.assertRaises(S3CopyFailedError) as changed:
                self.client.copy(
                    source,
                    destination["Bucket"],
                    destination["Key"],
                    ExtraArgs={"TaggingDirective": "COPY"},
                    Config=config,
                )

            failure = changed.exception.__context__
            self.assertIsInstance(failure, ClientError)
            assert isinstance(failure, ClientError)
            self.assertEqual(failure.response["Error"]["Code"], "PreconditionFailed")
        finally:
            self.client.meta.events.unregister(part_event, replace_source)
            self.client.meta.events.unregister(abort_event, observe_abort)

        self.assertEqual(parts, [1, 2])
        self.assertEqual(aborted, [204])
        preserved = peer.get_object(**destination, ChecksumMode="ENABLED")
        self.assertEqual(preserved["ETag"], previous["ETag"])
        self.assertEqual(preserved["Metadata"], {"generation": "previous"})

        with contextlib.closing(preserved["Body"]) as body:
            self.assertEqual(body.read(), b"previous destination")

        self.assertEqual(
            peer.get_object_tagging(**destination)["TagSet"],
            [{"Key": "version", "Value": "previous"}],
        )

        self.assertFalse(peer.list_multipart_uploads(Bucket="photos").get("Uploads"))

        self.client.copy(
            source,
            destination["Bucket"],
            destination["Key"],
            ExtraArgs={"TaggingDirective": "COPY"},
            Config=config,
        )

        copied = peer.get_object(**destination, ChecksumMode="ENABLED")
        self.assertEqual(copied["Metadata"], {"generation": "replacement"})

        with contextlib.closing(copied["Body"]) as body:
            actual = body.read()

        self.assertEqual(len(actual), len(replacement))

        self.assertEqual(
            hashlib.sha256(actual).digest(), hashlib.sha256(replacement).digest()
        )

        self.assertEqual(
            peer.get_object_tagging(**destination)["TagSet"],
            [{"Key": "version", "Value": "new"}],
        )

        self.assertFalse(peer.list_multipart_uploads(Bucket="photos").get("Uploads"))

    def test_low_level_defaults_and_managed_upload_copy(self) -> None:
        self.client.put_object(
            Bucket="photos",
            Key="boto-shared",
            Body=bytes(index % 251 for index in range(257 * 1024)),
        )

        target = {"Bucket": "photos", "Key": "boto-multipart /%+é"}

        created = self.client.create_multipart_upload(
            **target, Metadata={"source": "initiation"}, Tagging="kind=multipart"
        )

        upload = {**target, "UploadId": created["UploadId"]}
        part = self.client.upload_part(**upload, PartNumber=7, Body=b"abc")
        page = self.client.list_parts(**upload)
        self.assertEqual(page["Parts"][0]["ETag"], part["ETag"])
        self.assertEqual(page["ChecksumAlgorithm"], "CRC64NVME")

        result = self.client.complete_multipart_upload(
            **upload,
            MultipartUpload={"Parts": [{"PartNumber": 7, "ETag": part["ETag"]}]},
        )

        self.assertEqual(result["ChecksumCRC64NVME"], "BeXKuz/B+us=")
        self.assertTrue(result["Location"].startswith("http"))

        self.assertTrue(
            result["Location"].endswith("/photos/" + quote(target["Key"], safe=""))
        )

        attributes = self.client.get_object_attributes(
            **target, ObjectAttributes=["ETag", "ObjectSize", "Checksum", "ObjectParts"]
        )

        self.assertEqual(attributes["ObjectSize"], 3)
        self.assertEqual(attributes["ETag"], result["ETag"].strip('"'))
        self.assertEqual(attributes["ObjectParts"], {"TotalPartsCount": 1})
        self.check_presigned_layout(target)

        selected = self.client.get_object(
            **target, PartNumber=1, ChecksumMode="ENABLED"
        )

        with contextlib.closing(selected["Body"]) as body:
            self.assertEqual(selected["ResponseMetadata"]["HTTPStatusCode"], 206)
            self.assertEqual(selected["PartsCount"], 1)
            self.assertEqual(body.read(), b"abc")

        self.assertEqual(
            self.client.get_object_tagging(**target)["TagSet"],
            [{"Key": "kind", "Value": "multipart"}],
        )

        with self.assertRaises(ClientError) as completed:
            self.client.list_parts(**upload)

        self.assertEqual(completed.exception.response["Error"]["Code"], "NoSuchUpload")

        content = b"x" * (5 * 1024 * 1024) + b"y"

        config = TransferConfig(
            multipart_threshold=5 * 1024 * 1024,
            multipart_chunksize=5 * 1024 * 1024,
            max_concurrency=2,
            preferred_transfer_client="classic",
        )

        self.client.upload_fileobj(
            io.BytesIO(content),
            "photos",
            "boto-managed",
            ExtraArgs={"Metadata": {"source": "managed"}, "Tagging": "kind=managed"},
            Config=config,
        )

        self.client.copy(
            {"Bucket": "photos", "Key": "boto-managed"},
            "photos",
            "boto-managed-copy",
            ExtraArgs={"TaggingDirective": "COPY"},
            Config=config,
        )

        self.client.copy(
            target,
            "photos",
            "boto-small-managed-copy",
            Config=config,
        )

        small = self.client.get_object_attributes(
            Bucket="photos",
            Key="boto-small-managed-copy",
            ObjectAttributes=["ObjectSize", "ObjectParts"],
        )

        self.assertEqual(small["ObjectSize"], 3)
        self.assertNotIn("ObjectParts", small)

        for key in ("boto-managed", "boto-managed-copy"):
            selected = self.client.get_object(
                Bucket="photos", Key=key, PartNumber=2, ChecksumMode="ENABLED"
            )

            with contextlib.closing(selected["Body"]) as body:
                self.assertEqual(
                    selected["ContentRange"], "bytes 5242880-5242880/5242881"
                )

                self.assertEqual(selected["PartsCount"], 2)
                self.assertEqual(body.read(), b"y")

            head = self.client.head_object(Bucket="photos", Key=key, PartNumber=2)
            self.assertEqual(head["ResponseMetadata"]["HTTPStatusCode"], 206)
            self.assertEqual(head["ContentLength"], 1)

            with contextlib.closing(
                self.client.get_object(Bucket="photos", Key=key)["Body"]
            ) as body:
                self.assertEqual(body.read(), content)

            self.assertEqual(
                self.client.head_object(Bucket="photos", Key=key)["Metadata"],
                {"source": "managed"},
            )

            self.assertEqual(
                self.client.get_object_tagging(Bucket="photos", Key=key)["TagSet"],
                [{"Key": "kind", "Value": "managed"}],
            )

        self.assertFalse(
            self.client.list_multipart_uploads(Bucket="photos").get("Uploads")
        )

        # The managed helper applies tags after completion. Its later error
        # cannot roll back the already acknowledged destination object.
        denied_key = "boto-copy-tag-denied"

        self.client.put_bucket_policy(
            Bucket="photos",
            Policy=json.dumps(
                {
                    "Version": "2012-10-17",
                    "Statement": {
                        "Effect": "Deny",
                        "Principal": "*",
                        "Action": "s3:PutObjectTagging",
                        "Resource": f"arn:aws:s3:::photos/{denied_key}",
                    },
                }
            ),
        )

        try:
            with self.assertRaises(ClientError) as denied:
                self.client.copy(
                    {"Bucket": "photos", "Key": "boto-managed"},
                    "photos",
                    denied_key,
                    ExtraArgs={"TaggingDirective": "COPY"},
                    Config=config,
                )

            self.assertEqual(denied.exception.response["Error"]["Code"], "AccessDenied")

            with contextlib.closing(
                self.client.get_object(Bucket="photos", Key=denied_key)["Body"]
            ) as body:
                self.assertEqual(body.read(), content)

            self.assertFalse(
                self.client.list_multipart_uploads(Bucket="photos").get("Uploads")
            )
        finally:
            self.client.delete_bucket_policy(Bucket="photos")

        abandoned = self.client.create_multipart_upload(
            Bucket="photos", Key="boto-abandoned"
        )

        self.client.abort_multipart_upload(
            Bucket="photos", Key="boto-abandoned", UploadId=abandoned["UploadId"]
        )

    def check_presigned_layout(self, target: dict[str, str]) -> None:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        url = self.client.generate_presigned_url(
            "get_object_attributes",
            Params={
                **target,
                "ObjectAttributes": ["ObjectSize", "ObjectParts"],
                "MaxParts": 1,
                "PartNumberMarker": 0,
            },
            ExpiresIn=60,
        )

        headers = {
            "x-amz-object-attributes": "ObjectSize,ObjectParts",
            "x-amz-max-parts": "1",
            "x-amz-part-number-marker": "0",
        }

        with opener.open(
            urllib.request.Request(url, headers=headers), timeout=5
        ) as response:
            self.assertEqual(response.status, 200)
            payload = response.read()
            self.assertIn(b"<ObjectSize>3</ObjectSize>", payload)
            self.assertIn(b"<PartsCount>1</PartsCount>", payload)

        with self.assertRaises(urllib.error.HTTPError) as tampered:
            opener.open(
                urllib.request.Request(
                    url, headers={**headers, "x-amz-max-parts": "2"}
                ),
                timeout=5,
            )

        with tampered.exception as response:
            self.assertEqual(response.code, 403)

        for operation, method in [("get_object", "GET"), ("head_object", "HEAD")]:
            url = self.client.generate_presigned_url(
                operation,
                Params={**target, "PartNumber": 1, "ChecksumMode": "ENABLED"},
                ExpiresIn=60,
            )

            with opener.open(
                urllib.request.Request(
                    url, method=method, headers={"x-amz-checksum-mode": "ENABLED"}
                ),
                timeout=5,
            ) as response:
                self.assertEqual(response.status, 206)
                self.assertEqual(response.headers["Content-Range"], "bytes 0-2/3")

                self.assertEqual(
                    response.headers["x-amz-checksum-crc64nvme"], "BeXKuz/B+us="
                )

                self.assertEqual(response.read(), b"abc" if method == "GET" else b"")

            changed = url.replace("partNumber=1", "partNumber=2")
            self.assertNotEqual(changed, url)

            with self.assertRaises(urllib.error.HTTPError) as tampered:
                opener.open(
                    urllib.request.Request(
                        changed,
                        method=method,
                        headers={"x-amz-checksum-mode": "ENABLED"},
                    ),
                    timeout=5,
                )

            with tampered.exception as response:
                self.assertEqual(response.code, 403)
