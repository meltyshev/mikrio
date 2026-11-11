"""Pinned clients must interpret the complete deferred response, including errors."""

import datetime as dt

from botocore.exceptions import ClientError, ParamValidationError
from test_http_integrity import S3TestCase

ETAG = '"25f9e794323b453885f5181f1b624d0b"'


class PhaseTests(S3TestCase):
    def test_attributes_decode_selected_groups_count_only_and_part_metadata(
        self,
    ) -> None:
        modified = dt.datetime(2026, 10, 9, tzinfo=dt.UTC)

        result = self.client.get_object_attributes(
            Bucket="photos", Key="etag", ObjectAttributes=["ETag"]
        )

        self.assertEqual(result["ETag"], ETAG.strip('"'))
        self.assertEqual(result["LastModified"], modified)
        self.assertNotIn("ObjectSize", result)
        self.assertNotIn("ObjectParts", result)

        result = self.client.get_object_attributes(
            Bucket="photos", Key="count-only", ObjectAttributes=["ObjectParts"]
        )

        self.assertEqual(result["ObjectParts"], {"TotalPartsCount": 1})

        result = self.client.get_object_attributes(
            Bucket="photos",
            Key="part",
            ObjectAttributes=["ObjectParts", "Checksum", "StorageClass", "ObjectSize"],
            MaxParts=1,
            PartNumberMarker=0,
        )

        self.assertEqual(result["ObjectSize"], 9)
        self.assertEqual(result["StorageClass"], "STANDARD")
        self.assertNotIn("ETag", result)

        self.assertEqual(
            result["Checksum"],
            {"ChecksumCRC32": "y/Q5Jg==", "ChecksumType": "FULL_OBJECT"},
        )

        self.assertEqual(
            result["ObjectParts"],
            {
                "TotalPartsCount": 1,
                "MaxParts": 1,
                "PartNumberMarker": 0,
                "IsTruncated": False,
                "Parts": [{"PartNumber": 1, "Size": 9, "ChecksumCRC32": "y/Q5Jg=="}],
            },
        )

    def test_attributes_sdk_model_rejects_unmodeled_read_conditions(self) -> None:
        with self.assertRaises(ParamValidationError):
            self.client.get_object_attributes(
                Bucket="photos", Key="etag", ObjectAttributes=["ETag"], IfMatch="*"
            )

    # Botocore maps embedded HTTP-200 errors to 500 for retry/error handling.
    # Raw transport status is asserted independently by the Rust fixture.
    def test_copy_parses_success_and_errors_before_and_after_headers(self) -> None:
        success = self.client.copy_object(
            Bucket="photos", Key="deferred-success", CopySource="photos/source"
        )

        self.assertEqual(success["CopyObjectResult"]["ETag"], ETAG)
        self.assertEqual(success["ResponseMetadata"]["HTTPStatusCode"], 200)

        for key, code, status in [
            ("early-error", "NoSuchKey", 404),
            ("deferred-error", "InternalError", 500),
            ("stalled", "RequestTimeout", 500),
        ]:
            with self.subTest(key=key), self.assertRaises(ClientError) as raised:
                self.client.copy_object(
                    Bucket="photos", Key=key, CopySource="photos/source"
                )

            self.assertEqual(raised.exception.response["Error"]["Code"], code)

            self.assertEqual(
                raised.exception.response["ResponseMetadata"]["HTTPStatusCode"], status
            )

    def test_completion_parses_success_and_errors_before_and_after_headers(
        self,
    ) -> None:
        for key, code, status in [
            ("deferred-success", None, 200),
            ("early-error", "NoSuchKey", 404),
            ("deferred-error", "InternalError", 500),
            ("stalled", "RequestTimeout", 500),
        ]:
            with self.subTest(key=key):
                arguments = {
                    "Bucket": "photos",
                    "Key": key,
                    "UploadId": "synthetic-upload",
                    "MultipartUpload": {"Parts": [{"PartNumber": 1, "ETag": ETAG}]},
                }

                if code is None:
                    result = self.client.complete_multipart_upload(**arguments)
                    self.assertEqual(result["ETag"], ETAG)
                    continue

                with self.assertRaises(ClientError) as raised:
                    self.client.complete_multipart_upload(**arguments)

                self.assertEqual(raised.exception.response["Error"]["Code"], code)

                self.assertEqual(
                    raised.exception.response["ResponseMetadata"]["HTTPStatusCode"],
                    status,
                )

    def test_part_copy_sends_a_valid_empty_body(self) -> None:
        result = self.client.upload_part_copy(
            Bucket="photos",
            Key="part-copy",
            UploadId="synthetic-upload",
            PartNumber=1,
            CopySource="photos/source",
        )

        self.assertEqual(result["CopyPartResult"]["ETag"], ETAG)
