"""Real SDK traffic through public listeners and a provisioned scoped key."""

import base64
import contextlib
import hashlib
import io
import json
import os
import ssl
import time
import urllib.error
import urllib.request
from datetime import UTC, datetime, timedelta
from typing import Any
from unittest.mock import patch

from boto3.s3.transfer import TransferConfig
from botocore.exceptions import ClientError, ConnectionClosedError
from http_loss import lost_response
from test_http_integrity import S3TestCase
from test_http_storage import StorageTests


class ListenerTests(StorageTests):
    def test_lost_put_response_preserves_commit_and_explicit_retry_overwrites_peer(
        self,
    ) -> None:
        target = {"Bucket": "photos", "Key": "boto-lost-put"}
        self.addCleanup(self.client.delete_object, **target)
        payload = b"caller A bytes"
        checksum = base64.b64encode(hashlib.sha256(payload).digest()).decode()

        self.client.put_object(
            **target, Body=b"previous bytes", Tagging="version=previous"
        )

        def inspect(expected: bytes, tag: str) -> None:
            response = self.client.get_object(**target, ChecksumMode="ENABLED")

            with contextlib.closing(response["Body"]) as body:
                self.assertEqual(body.read(), expected)

            self.assertEqual(
                self.client.get_object_tagging(**target)["TagSet"],
                [{"Key": "version", "Value": tag}],
            )

        with lost_response("PUT", "/photos/boto-lost-put") as (endpoint, proxy):
            client = self.client_for(endpoint)
            source = io.BytesIO(payload)

            with self.assertRaises(ConnectionClosedError):
                client.put_object(
                    **target,
                    Body=source,
                    ChecksumSHA256=checksum,
                    Tagging="version=A",
                )

            self.assertEqual(source.tell(), len(payload))
            self.assertEqual(proxy.attempts, 1)
            self.assertEqual(len(proxy.observations), 1)
            self.assertEqual(proxy.observations[0]["status"], 200)
            self.assertEqual(proxy.observations[0]["encoded_bytes"], len(payload))

        inspect(payload, "A")
        self.client.put_object(**target, Body=b"peer B bytes", Tagging="version=B")
        inspect(b"peer B bytes", "B")

        self.client.put_object(
            **target,
            Body=io.BytesIO(payload),
            ChecksumSHA256=checksum,
            Tagging="version=A",
        )

        inspect(payload, "A")

    def test_request_ids_match_sdk_error_metadata_and_independent_requests(
        self,
    ) -> None:
        target = {"Bucket": "photos", "Key": "boto-request-identity"}
        self.addCleanup(self.client.delete_object, **target)
        created = self.client.put_object(**target, Body=b"request identity")
        request_id = created["ResponseMetadata"]["RequestId"]
        self.assertRegex(request_id, r"^[0-9a-f]{32}$")

        with self.assertRaises(ClientError) as rejected:
            self.client.get_object(**target, IfMatch='"not-current"')

        failed = rejected.exception.response["ResponseMetadata"]
        self.assertEqual(failed["HTTPStatusCode"], 412)
        self.assertRegex(failed["RequestId"], r"^[0-9a-f]{32}$")
        self.assertNotEqual(failed["RequestId"], request_id)
        self.assertEqual(failed["RequestId"], failed["HTTPHeaders"]["x-amz-request-id"])

    def test_sdk_signatures_enforce_skew_and_presigned_expiry(self) -> None:
        target = {"Bucket": "photos", "Key": "boto-signed-time"}
        self.client.put_object(**target, Body=b"signed time")
        self.addCleanup(self.client.delete_object, **target)

        for offset, expected in [
            (-910, "RequestTimeTooSkewed"),
            (-890, None),
            (890, None),
            (910, "RequestTimeTooSkewed"),
        ]:
            with self.subTest(mode="header", offset=offset):
                signed_at = datetime.now(UTC) + timedelta(seconds=offset)

                # Change only the SDK's signing input, never the engine clock.
                with patch(
                    "botocore.auth.get_current_datetime", return_value=signed_at
                ):
                    if expected is not None:
                        with self.assertRaises(ClientError) as denied:
                            self.client.get_object(**target)

                        self.assertEqual(
                            denied.exception.response["Error"]["Code"], expected
                        )

                        continue

                    response = self.client.get_object(**target)

                    with contextlib.closing(response["Body"]) as body:
                        self.assertEqual(body.read(), b"signed time")

        opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPSHandler(
                context=ssl.create_default_context(
                    cafile=os.environ.get("mikrio_fixture_tls_ca")
                )
            ),
        )

        for offset, expected in [
            (-70, "AccessDenied"),
            (-50, None),
            (890, None),
            (910, "RequestTimeTooSkewed"),
        ]:
            with self.subTest(mode="query", offset=offset):
                signed_at = datetime.now(UTC) + timedelta(seconds=offset)

                with patch(
                    "botocore.auth.get_current_datetime", return_value=signed_at
                ):
                    url = self.client.generate_presigned_url(
                        "get_object", Params=target, ExpiresIn=60
                    )

                if expected is not None:
                    with self.assertRaises(urllib.error.HTTPError) as denied_url:
                        opener.open(url, timeout=5)

                    with denied_url.exception as response:
                        self.assertEqual(response.code, 403)

                        self.assertIn(
                            f"<Code>{expected}</Code>".encode(), response.read()
                        )

                    continue

                with opener.open(url, timeout=5) as response:
                    self.assertEqual(response.status, 200)
                    self.assertEqual(response.read(), b"signed time")

    def test_selected_download_finishes_after_real_url_expiry_and_new_read_is_denied(
        self,
    ) -> None:
        target = {"Bucket": "photos", "Key": "boto-expiring-download"}
        length = 64 * 1024 * 1024
        self.client.put_object(**target, Body=b"x" * length)
        self.addCleanup(self.client.delete_object, **target)

        opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPSHandler(
                context=ssl.create_default_context(
                    cafile=os.environ.get("mikrio_fixture_tls_ca")
                )
            ),
        )

        url = self.client.generate_presigned_url(
            "get_object", Params=target, ExpiresIn=2
        )

        with opener.open(url, timeout=5) as response:
            self.assertEqual(response.status, 200)
            first = response.read(64 * 1024)
            self.assertEqual(first, b"x" * (64 * 1024))
            # A real elapsed interval exceeds the signed two-second lifetime.
            started = time.monotonic()
            time.sleep(3)
            self.assertGreaterEqual(time.monotonic() - started, 3)

            with self.assertRaises(urllib.error.HTTPError) as expired:
                opener.open(url, timeout=5)

            with expired.exception as rejected:
                self.assertEqual(rejected.code, 403)
                self.assertIn(b"<Code>AccessDenied</Code>", rejected.read())

            received = len(first)

            while chunk := response.read(64 * 1024):
                self.assertEqual(chunk, b"x" * len(chunk))
                received += len(chunk)

            self.assertEqual(received, length)

    def test_operation_rejections_preserve_bucket_configuration_parts_and_bytes(
        self,
    ) -> None:
        bucket = "listener-python"
        self.client.create_bucket(Bucket=bucket)
        self.addCleanup(self.client.delete_bucket, Bucket=bucket)
        target = {"Bucket": bucket, "Key": "operation-errors"}

        self.client.put_object(
            **target,
            Body=b"original",
            Tagging="kind=kept",
            Metadata={"origin": "kept"},
        )

        self.addCleanup(self.client.delete_object, **target)

        upload = {
            **target,
            "UploadId": self.client.create_multipart_upload(**target)["UploadId"],
        }

        self.addCleanup(self.client.abort_multipart_upload, **upload)
        part = self.client.upload_part(**upload, PartNumber=1, Body=b"abc")
        peer = self.client_for(os.environ["mikrio_fixture_peer"])
        wrong_owner = "000000000000"

        try:
            peer.get_bucket_location(Bucket=bucket, ExpectedBucketOwner=wrong_owner)
        except ClientError as error:
            self.assertEqual(error.response["Error"]["Code"], "AccessDenied")
        else:
            wrong_owner = "111111111111"

        configuration = {
            "Rules": [
                {
                    "ID": "kept",
                    "Status": "Disabled",
                    "Filter": {"Prefix": "never/"},
                    "Expiration": {"Days": 1},
                }
            ]
        }

        cors = {
            "CORSRules": [
                {"AllowedOrigins": ["https://app.example"], "AllowedMethods": ["GET"]}
            ]
        }

        flags = {
            "BlockPublicAcls": True,
            "IgnorePublicAcls": True,
            "BlockPublicPolicy": True,
            "RestrictPublicBuckets": True,
        }

        policy = json.dumps(
            {
                "Version": "2012-10-17",
                "Statement": {
                    "Effect": "Deny",
                    "Principal": "*",
                    "Action": "s3:GetObject",
                    "Resource": f"arn:aws:s3:::{bucket}/never/*",
                },
            }
        )

        ownership = {"Rules": [{"ObjectOwnership": "BucketOwnerEnforced"}]}
        self.client.put_bucket_cors(Bucket=bucket, CORSConfiguration=cors)

        self.client.put_bucket_lifecycle_configuration(
            Bucket=bucket, LifecycleConfiguration=configuration
        )

        self.client.put_bucket_policy(Bucket=bucket, Policy=policy)
        replacement_rule = {**configuration["Rules"][0], "ID": "rejected"}

        operations: list[tuple[str, dict[str, Any]]] = [
            ("head_bucket", {"Bucket": bucket}),
            ("delete_bucket", {"Bucket": bucket}),
            ("get_bucket_location", {"Bucket": bucket}),
            (
                "put_bucket_cors",
                {
                    "Bucket": bucket,
                    "CORSConfiguration": {
                        "CORSRules": [
                            {
                                "AllowedOrigins": ["https://rejected.example"],
                                "AllowedMethods": ["HEAD"],
                            }
                        ]
                    },
                },
            ),
            ("delete_bucket_cors", {"Bucket": bucket}),
            (
                "put_bucket_lifecycle_configuration",
                {
                    "Bucket": bucket,
                    "LifecycleConfiguration": {"Rules": [replacement_rule]},
                },
            ),
            ("delete_bucket_lifecycle", {"Bucket": bucket}),
            (
                "copy_object",
                {**target, "CopySource": target, "MetadataDirective": "REPLACE"},
            ),
            ("create_multipart_upload", target),
            ("list_multipart_uploads", {"Bucket": bucket}),
            (
                "complete_multipart_upload",
                {
                    **upload,
                    "MultipartUpload": {
                        "Parts": [{"PartNumber": 1, "ETag": part["ETag"]}]
                    },
                },
            ),
            ("abort_multipart_upload", upload),
            (
                "put_bucket_policy",
                {"Bucket": bucket, "Policy": policy.replace("/never/", "/changed/")},
            ),
            ("delete_bucket_policy", {"Bucket": bucket}),
            ("get_bucket_policy_status", {"Bucket": bucket}),
            (
                "put_public_access_block",
                {
                    "Bucket": bucket,
                    "PublicAccessBlockConfiguration": {name: False for name in flags},
                },
            ),
            ("delete_public_access_block", {"Bucket": bucket}),
            (
                "put_bucket_ownership_controls",
                {"Bucket": bucket, "OwnershipControls": ownership},
            ),
            ("get_bucket_ownership_controls", {"Bucket": bucket}),
            ("get_bucket_acl", {"Bucket": bucket}),
            ("get_object_acl", target),
            ("get_object_tagging", target),
            ("delete_object_tagging", target),
            ("upload_part_copy", {**upload, "PartNumber": 1, "CopySource": target}),
        ]

        for operation, fields in operations:
            with self.subTest(operation=operation):
                with self.assertRaises(ClientError) as caught:
                    getattr(self.client, operation)(
                        **fields, ExpectedBucketOwner=wrong_owner
                    )

                response = caught.exception.response
                self.assertEqual(response["ResponseMetadata"]["HTTPStatusCode"], 403)

                self.assertEqual(
                    response["Error"]["Code"],
                    "403" if operation == "head_bucket" else "AccessDenied",
                )

        for operation, fields, code, status in [
            ("create_bucket", {"Bucket": "ungranted-bucket"}, "AccessDenied", 403),
            ("list_buckets", {"ContinuationToken": "invalid"}, "InvalidArgument", 400),
        ]:
            with self.subTest(operation=operation):
                with self.assertRaises(ClientError) as caught:
                    getattr(self.client, operation)(**fields)

                self.assertEqual(caught.exception.response["Error"]["Code"], code)

                self.assertEqual(
                    caught.exception.response["ResponseMetadata"]["HTTPStatusCode"],
                    status,
                )

        self.assertNotIn(
            "ungranted-bucket",
            [entry["Name"] for entry in peer.list_buckets()["Buckets"]],
        )

        self.assertEqual(peer.get_bucket_policy(Bucket=bucket)["Policy"], policy)

        self.assertEqual(
            peer.get_bucket_cors(Bucket=bucket)["CORSRules"], cors["CORSRules"]
        )

        self.assertEqual(
            peer.get_bucket_lifecycle_configuration(Bucket=bucket)["Rules"],
            configuration["Rules"],
        )

        self.assertEqual(
            peer.get_public_access_block(Bucket=bucket)[
                "PublicAccessBlockConfiguration"
            ],
            flags,
        )

        self.assertEqual(
            peer.get_bucket_ownership_controls(Bucket=bucket)["OwnershipControls"],
            ownership,
        )

        self.assertEqual(
            peer.get_object_tagging(**target)["TagSet"],
            [{"Key": "kind", "Value": "kept"}],
        )

        uploads = peer.list_multipart_uploads(Bucket=bucket)["Uploads"]

        self.assertEqual([entry["UploadId"] for entry in uploads], [upload["UploadId"]])

        parts = peer.list_parts(**upload)["Parts"]
        self.assertEqual(len(parts), 1)
        self.assertEqual(parts[0]["ETag"], part["ETag"])
        response = peer.get_object(**target)

        with contextlib.closing(response["Body"]) as body:
            self.assertEqual(response["Metadata"], {"origin": "kept"})
            self.assertEqual(body.read(), b"original")

        self.client.delete_object_tagging(**target)
        self.assertEqual(peer.get_object_tagging(**target)["TagSet"], [])
        response = peer.get_object(**target)

        with contextlib.closing(response["Body"]) as body:
            self.assertEqual(body.read(), b"original")

    def test_batch_deletion_uses_sdk_digest_conditions_duplicates_and_quiet(
        self,
    ) -> None:
        target = {"Bucket": "photos", "Key": "boto-batch"}
        self.addCleanup(self.client.delete_object, **target)
        put = self.client.put_object(**target, Body=b"payload")
        peer = self.client_for(os.environ["mikrio_fixture_peer"])

        output = peer.delete_objects(
            Bucket="photos",
            Delete={
                "Objects": [
                    {"Key": target["Key"], "ETag": '"wrong"'},
                    {"Key": target["Key"], "ETag": put["ETag"]},
                    {"Key": target["Key"], "ETag": put["ETag"]},
                    {"Key": target["Key"]},
                ]
            },
        )

        self.assertEqual(output["ResponseMetadata"]["HTTPStatusCode"], 200)

        self.assertEqual(
            [deleted["Key"] for deleted in output["Deleted"]], [target["Key"]] * 2
        )

        self.assertEqual(
            [error["Code"] for error in output["Errors"]],
            ["PreconditionFailed", "NoSuchKey"],
        )

        quiet = peer.delete_objects(
            Bucket="photos",
            Delete={
                "Quiet": True,
                "Objects": [
                    {"Key": target["Key"]},
                    {"Key": target["Key"], "ETag": "*"},
                ],
            },
        )

        self.assertNotIn("Deleted", quiet)
        self.assertEqual(quiet["Errors"][0]["Code"], "NoSuchKey")

        with self.assertRaises(ClientError) as caught:
            self.client.head_object(**target)

        self.assertEqual(
            caught.exception.response["ResponseMetadata"]["HTTPStatusCode"], 404
        )

    def test_bucket_paginator_preserves_account_listing_and_region_fields(self) -> None:
        for bucket in ("listener-node", "listener-python"):
            self.client.create_bucket(Bucket=bucket)
            self.addCleanup(self.client.delete_bucket, Bucket=bucket)

        peer = self.client_for(os.environ["mikrio_fixture_peer"])

        pages = list(
            peer.get_paginator("list_buckets").paginate(
                Prefix="listener-", PaginationConfig={"PageSize": 1}
            )
        )

        self.assertEqual(len(pages), 2)

        self.assertEqual(
            [bucket["Name"] for page in pages for bucket in page["Buckets"]],
            ["listener-node", "listener-python"],
        )

        for page in pages:
            self.assertEqual(page["Prefix"], "listener-")
            self.assertEqual(page["Buckets"][0]["BucketRegion"], "us-east-1")
            self.assertIsInstance(page["Buckets"][0]["CreationDate"], datetime)
            self.assertEqual(len(page["Owner"]["ID"]), 64)
            self.assertNotIn("DisplayName", page["Owner"])
            self.assertNotIn("IsTruncated", page)

        self.assertIn("ContinuationToken", pages[0])
        self.assertNotIn("ContinuationToken", pages[1])
        complete = peer.list_buckets()
        self.assertIn("photos", [bucket["Name"] for bucket in complete["Buckets"]])
        self.assertNotIn("ContinuationToken", complete)

        for bucket in complete["Buckets"]:
            self.assertNotIn("BucketRegion", bucket)

    def test_ranges_overrides_and_managed_download_use_shared_reads(self) -> None:
        bucket = "listener-python"
        key = "ranged-download"
        self.client.create_bucket(Bucket=bucket)
        self.addCleanup(self.client.delete_bucket, Bucket=bucket)
        content = bytes(range(256)) * 32_768

        self.client.put_object(
            Bucket=bucket, Key=key, Body=content, ContentType="text/plain"
        )

        self.addCleanup(self.client.delete_object, Bucket=bucket, Key=key)
        peer = self.client_for(os.environ["mikrio_fixture_peer"])
        expires = datetime(2037, 10, 21, 7, 28, tzinfo=UTC)

        for method in (peer.get_object, peer.head_object):
            output = method(
                Bucket=bucket,
                Key=key,
                Range="bytes=4-7",
                ChecksumMode="ENABLED",
                ResponseContentType="application/pdf",
                ResponseContentEncoding="identity",
                ResponseContentDisposition="attachment",
                ResponseContentLanguage="",
                ResponseCacheControl="no-store",
                ResponseExpires=expires,
            )

            if "Body" in output:
                self.addCleanup(output["Body"].close)

            self.assertEqual(output["ContentLength"], 4)
            self.assertEqual(output["ContentRange"], f"bytes 4-7/{len(content)}")
            self.assertEqual(output["ContentType"], "application/pdf")
            self.assertEqual(output["ContentEncoding"], "identity")
            self.assertEqual(output["ContentDisposition"], "attachment")
            self.assertEqual(output["ContentLanguage"], "")
            self.assertEqual(output["CacheControl"], "no-store")
            self.assertEqual(output["Expires"], expires)
            self.assertNotIn("ChecksumCRC32", output)
            self.assertNotIn("ChecksumType", output)

            if "Body" in output:
                with contextlib.closing(output["Body"]) as body:
                    self.assertEqual(body.read(), content[4:8])

        target = io.BytesIO()

        peer.download_fileobj(
            bucket,
            key,
            target,
            Config=TransferConfig(
                multipart_threshold=1,
                multipart_chunksize=5 * 1024 * 1024,
                max_concurrency=2,
                preferred_transfer_client="classic",
            ),
        )

        self.assertEqual(target.getvalue(), content)
        original = peer.head_object(Bucket=bucket, Key=key)
        self.assertEqual(original["ContentType"], "text/plain")
        self.assertEqual(original["ContentLength"], len(content))

    def test_listing_paginator_keeps_prefix_only_pages_and_literal_keys(self) -> None:
        bucket = "listener-python"
        self.client.create_bucket(Bucket=bucket)
        self.addCleanup(self.client.delete_bucket, Bucket=bucket)
        keys = ["a/one", "a/two", "b +雪", "c"]

        for key in keys:
            self.client.put_object(Bucket=bucket, Key=key, Body=b"abc")
            self.addCleanup(self.client.delete_object, Bucket=bucket, Key=key)

        peer = self.client_for(os.environ["mikrio_fixture_peer"])

        pages = list(
            peer.get_paginator("list_objects_v2").paginate(
                Bucket=bucket, Delimiter="/", PaginationConfig={"PageSize": 1}
            )
        )

        self.assertEqual(len(pages), 3)
        self.assertEqual(pages[0].get("Contents", []), [])
        self.assertEqual(pages[0]["CommonPrefixes"], [{"Prefix": "a/"}])
        self.assertTrue(pages[0]["IsTruncated"])
        self.assertEqual(pages[1]["Contents"][0]["Key"], "b +雪")
        self.assertEqual(pages[1]["Contents"][0]["Size"], 3)

        self.assertEqual(
            pages[1]["Contents"][0]["ETag"], '"900150983cd24fb0d6963f7d28e17f72"'
        )

        self.assertEqual(pages[2]["Contents"][0]["Key"], "c")
        self.assertFalse(pages[2]["IsTruncated"])
        self.assertNotIn("NextContinuationToken", pages[2])
        self.assertTrue(all(page["KeyCount"] == 1 for page in pages))

        with self.assertRaises(ClientError) as changed:
            peer.list_objects_v2(
                Bucket=bucket, ContinuationToken=pages[0]["NextContinuationToken"]
            )

        self.assertEqual(changed.exception.response["Error"]["Code"], "InvalidArgument")

    def test_cors_round_trip_preflight_and_presigned_requests_across_listeners(
        self,
    ) -> None:
        bucket = "listener-python"
        self.client.create_bucket(Bucket=bucket)
        self.addCleanup(self.client.delete_bucket, Bucket=bucket)
        peer = self.client_for(os.environ["mikrio_fixture_peer"])

        rules = [
            {
                "ID": "first",
                "AllowedOrigins": ["https://app.example"],
                "AllowedMethods": ["PUT"],
                "AllowedHeaders": ["x-first"],
                "ExposeHeaders": ["x-first"],
            },
            {
                "ID": "browser-😀",
                "AllowedOrigins": ["https://app.example"],
                "AllowedMethods": ["PUT", "GET", "HEAD"],
                "AllowedHeaders": ["content-type"],
                "ExposeHeaders": ["ETag"],
                "MaxAgeSeconds": -(2**31),
            },
        ]

        self.client.put_bucket_cors(
            Bucket=bucket, CORSConfiguration={"CORSRules": rules}
        )

        self.assertEqual(peer.get_bucket_cors(Bucket=bucket)["CORSRules"], rules)
        self.client.put_object(Bucket=bucket, Key="cors", Body=b"private-cors")
        self.addCleanup(self.client.delete_object, Bucket=bucket, Key="cors")
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        for endpoint in [
            os.environ["mikrio_fixture_endpoint"],
            os.environ["mikrio_fixture_peer"],
        ]:
            url = f"{endpoint}/{bucket}/cors"

            preflight = urllib.request.Request(
                url,
                method="OPTIONS",
                headers={
                    "Origin": "https://app.example",
                    "Access-Control-Request-Method": "PUT",
                    "Access-Control-Request-Headers": "Content-Type",
                },
            )

            with opener.open(preflight, timeout=5) as response:
                self.assertEqual(response.status, 200)
                self.assertEqual(response.read(), b"")

                self.assertEqual(
                    response.headers["Access-Control-Allow-Origin"],
                    "https://app.example",
                )

                self.assertEqual(
                    response.headers["Access-Control-Expose-Headers"], "ETag"
                )

                self.assertEqual(
                    response.headers["Access-Control-Max-Age"], str(-(2**31))
                )

            signed = self.client_for(endpoint).generate_presigned_url(
                "put_object",
                Params={"Bucket": bucket, "Key": "cors", "ContentType": "text/plain"},
                ExpiresIn=60,
            )

            with opener.open(
                urllib.request.Request(
                    signed, method="OPTIONS", headers=dict(preflight.header_items())
                ),
                timeout=5,
            ) as response:
                self.assertEqual(response.status, 200)
                self.assertEqual(response.read(), b"")

                self.assertEqual(
                    response.headers["Access-Control-Allow-Origin"],
                    "https://app.example",
                )

            with self.assertRaises(urllib.error.HTTPError) as invalid:
                opener.open(
                    urllib.request.Request(
                        signed,
                        method="PUT",
                        data=b"invalid-signature",
                        headers={"Content-Type": "application/octet-stream"},
                    ),
                    timeout=5,
                )

            with invalid.exception as response:
                self.assertEqual(response.code, 403)
                self.assertIn(b"<Code>SignatureDoesNotMatch</Code>", response.read())

            with opener.open(
                urllib.request.Request(
                    signed,
                    method="PUT",
                    data=b"private-cors",
                    headers={
                        "Origin": "https://app.example",
                        "Content-Type": "text/plain",
                    },
                ),
                timeout=5,
            ) as response:
                self.assertEqual(response.status, 200)

            with self.assertRaises(urllib.error.HTTPError) as denied:
                opener.open(
                    urllib.request.Request(
                        url, headers={"Origin": "https://app.example"}
                    ),
                    timeout=5,
                )

            with denied.exception as response:
                self.assertEqual(response.code, 403)

                self.assertEqual(
                    response.headers["Access-Control-Allow-Origin"],
                    "https://app.example",
                )

                self.assertIn(b"<Code>AccessDenied</Code>", response.read())

        signed = peer.generate_presigned_url(
            "get_object", Params={"Bucket": bucket, "Key": "cors"}, ExpiresIn=60
        )

        signed_request = urllib.request.Request(
            signed, headers={"Origin": "https://app.example"}
        )

        with opener.open(signed_request, timeout=5) as response:
            self.assertEqual(response.read(), b"private-cors")
            self.assertEqual(response.headers["Access-Control-Expose-Headers"], "ETag")

        for _ in range(2):
            self.client.delete_bucket_cors(Bucket=bucket)

        with self.assertRaises(ClientError) as missing:
            peer.get_bucket_cors(Bucket=bucket)

        self.assertEqual(
            missing.exception.response["Error"]["Code"], "NoSuchCORSConfiguration"
        )

        with opener.open(signed_request, timeout=5) as response:
            self.assertEqual(response.read(), b"private-cors")
            self.assertIsNone(response.headers.get("Access-Control-Allow-Origin"))

    def test_configuration_and_public_restriction_across_live_listeners(self) -> None:
        bucket = "listener-python"
        self.client.create_bucket(Bucket=bucket)
        self.addCleanup(self.client.delete_bucket, Bucket=bucket)
        peer = self.client_for(os.environ["mikrio_fixture_peer"])

        flags = {
            "BlockPublicAcls": False,
            "IgnorePublicAcls": False,
            "BlockPublicPolicy": False,
            "RestrictPublicBuckets": False,
        }

        self.client.put_public_access_block(
            Bucket=bucket, PublicAccessBlockConfiguration=flags
        )

        self.assertEqual(
            peer.get_public_access_block(Bucket=bucket)[
                "PublicAccessBlockConfiguration"
            ],
            flags,
        )

        document = json.dumps(
            {
                "Version": "2012-10-17",
                "Statement": {
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:GetObject",
                    "Resource": f"arn:aws:s3:::{bucket}/*",
                },
            }
        )

        self.client.put_bucket_policy(Bucket=bucket, Policy=document)
        self.assertEqual(peer.get_bucket_policy(Bucket=bucket)["Policy"], document)

        self.assertTrue(
            peer.get_bucket_policy_status(Bucket=bucket)["PolicyStatus"]["IsPublic"]
        )

        controls = {"Rules": [{"ObjectOwnership": "BucketOwnerEnforced"}]}

        self.client.put_bucket_ownership_controls(
            Bucket=bucket, OwnershipControls=controls
        )

        self.assertEqual(
            peer.get_bucket_ownership_controls(Bucket=bucket)["OwnershipControls"],
            controls,
        )

        self.client.put_object(Bucket=bucket, Key="public", Body=b"visible")
        self.addCleanup(self.client.delete_object, Bucket=bucket, Key="public")
        acl = peer.get_bucket_acl(Bucket=bucket)
        object_acl = peer.get_object_acl(Bucket=bucket, Key="public")
        self.assertEqual(object_acl["Owner"], acl["Owner"])

        self.assertEqual(
            object_acl["Grants"],
            [
                {
                    "Grantee": {"ID": acl["Owner"]["ID"], "Type": "CanonicalUser"},
                    "Permission": "FULL_CONTROL",
                }
            ],
        )

        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        urls = [
            f"{os.environ[name]}/{bucket}/public"
            for name in ("mikrio_fixture_endpoint", "mikrio_fixture_peer")
        ]

        for url in urls:
            with opener.open(url, timeout=5) as response:
                self.assertEqual(response.read(), b"visible")

        self.client.put_public_access_block(
            Bucket=bucket,
            PublicAccessBlockConfiguration={**flags, "RestrictPublicBuckets": True},
        )

        for url in urls:
            with self.assertRaises(urllib.error.HTTPError) as denied:
                opener.open(url, timeout=5)

            with denied.exception as response:
                self.assertEqual(response.code, 403)
                self.assertIn(b"<Code>AccessDenied</Code>", response.read())

        # Classification describes the policy independently of its effective BPA restriction.
        self.assertTrue(
            peer.get_bucket_policy_status(Bucket=bucket)["PolicyStatus"]["IsPublic"]
        )

        for _ in range(2):
            self.client.delete_bucket_policy(Bucket=bucket)
            self.client.delete_public_access_block(Bucket=bucket)

        for operation, code in [
            (peer.get_bucket_policy, "NoSuchBucketPolicy"),
            (peer.get_public_access_block, "NoSuchPublicAccessBlockConfiguration"),
        ]:
            with self.assertRaises(ClientError) as missing:
                operation(Bucket=bucket)

            self.assertEqual(missing.exception.response["Error"]["Code"], code)

    def test_bucket_routes_and_scoped_denial_use_current_authority(self) -> None:
        self.client.create_bucket(Bucket="listener-python")
        self.addCleanup(self.client.delete_bucket, Bucket="listener-python")
        self.client.head_bucket(Bucket="listener-python")
        location = self.client.get_bucket_location(Bucket="listener-python")
        self.assertIsNone(location["LocationConstraint"])

        peer = self.client_for(os.environ["mikrio_fixture_peer"])
        peer.head_bucket(Bucket="listener-python")
        response = peer.get_object(Bucket="photos", Key="native-seed")

        with contextlib.closing(response["Body"]) as body:
            self.assertEqual(body.read(), b"native-to-sdk")

        for client in [self.client, peer]:
            with self.assertRaises(ClientError) as denied:
                client.put_object(Bucket="photos", Key="forbidden", Body=b"denied")

            self.assertEqual(denied.exception.response["Error"]["Code"], "AccessDenied")


class RevokedTests(S3TestCase):
    def test_independent_process_revocation_denies_both_live_listeners(self) -> None:
        peer = self.client_for(os.environ["mikrio_fixture_peer"])

        for client in [self.client, peer]:
            with self.assertRaises(ClientError) as denied:
                client.get_object(Bucket="photos", Key="native-seed")

            self.assertEqual(denied.exception.response["Error"]["Code"], "AccessDenied")

            self.assertEqual(
                denied.exception.response["ResponseMetadata"]["HTTPStatusCode"], 403
            )
