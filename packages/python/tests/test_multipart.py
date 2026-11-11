"""COPY and resumable multipart through an installed wheel's public API."""

import tempfile
import unittest
from collections import UserDict
from collections.abc import Callable, Iterator
from pathlib import Path
from types import MappingProxyType

from mikrio import CompletedPart, ObjectRef, StorageError, Store, Tag


class MultipartTests(unittest.TestCase):
    def test_copy_resume_stream_failure_and_detached_results(self) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-multipart-") as temporary:
            directory = Path(temporary)

            with Store.open(directory=directory) as store:
                store.create_bucket(bucket="photos")
                store.put_object(bucket="photos", key="source", body=b"abc")

                copied = store.copy_object(
                    bucket="photos",
                    key="copied",
                    copy_source=ObjectRef(bucket="photos", key="source"),
                    metadata_directive="REPLACE",
                    metadata=MappingProxyType({"origin": "copy"}),
                    tagging_directive="REPLACE",
                    tags=[Tag(key="kind", value="copy")],
                )

                created = store.create_multipart_upload(
                    bucket="photos",
                    key="object",
                    metadata=UserDict({"origin": "initiation"}),
                    tags=[Tag(key="kind", value="multipart")],
                )

                self.assertEqual(created.checksum_algorithm, "CRC64NVME")
                self.assertEqual(created.checksum_type, "FULL_OBJECT")

                self.assertEqual(
                    store.head_object(bucket="photos", key="copied").metadata,
                    {"origin": "copy"},
                )

                for invalid in (
                    UserDict({str(index): "value" for index in range(65)}),
                    UserDict({"origin": "first", "Origin": "second"}),
                ):
                    with self.subTest(metadata=invalid):
                        with self.assertRaises(StorageError) as rejected:
                            store.create_multipart_upload(
                                bucket="photos", key="invalid", metadata=invalid
                            )

                        self.assertEqual(rejected.exception.code, "InvalidArgument")

                        self.assertEqual(
                            rejected.exception.mutation_outcome, "NotCommitted"
                        )

            with Store.open(directory=directory) as peer:
                active = peer.list_multipart_uploads(bucket="photos", max_uploads=1)
                self.assertFalse(active.is_truncated)
                self.assertEqual(active.uploads[0].upload_id, created.upload_id)

                part = peer.upload_part(
                    bucket="photos",
                    key="object",
                    upload_id=created.upload_id,
                    part_number=7,
                    body=iter([b"a", b"", b"bc"]),
                    content_length=3,
                    checksum_crc32="NSRBwg==",
                )

                self.assertEqual(part.checksum_crc64_nvme, "BeXKuz/B+us=")
                failure = RuntimeError("producer failure")

                def broken() -> Iterator[bytes]:
                    yield b"before failure"
                    raise failure

                with self.assertRaises(StorageError) as failed:
                    peer.upload_part(
                        bucket="photos",
                        key="object",
                        upload_id=created.upload_id,
                        part_number=7,
                        body=broken(),
                    )

                self.assertEqual(failed.exception.code, "BodyReadFailed")
                self.assertIs(failed.exception.__cause__, failure)
                self.assertEqual(failed.exception.mutation_outcome, "NotCommitted")

                page = peer.list_parts(
                    bucket="photos", key="object", upload_id=created.upload_id
                )

                self.assertEqual(page.parts[0].etag, part.etag)

                replacement = peer.upload_part_copy(
                    bucket="photos",
                    key="object",
                    upload_id=created.upload_id,
                    part_number=7,
                    copy_source=ObjectRef(bucket="photos", key="copied"),
                    copy_source_if_match=copied.etag,
                )

                complete = peer.complete_multipart_upload(
                    bucket="photos",
                    key="object",
                    upload_id=created.upload_id,
                    parts=[CompletedPart(part_number=7, etag=replacement.etag)],
                    content_length=3,
                    checksum_crc64_nvme="BeXKuz/B+us=",
                )

                self.assertEqual(complete.checksum_type, "FULL_OBJECT")
                self.assertTrue(complete.etag.endswith('-1"'))

                count_only = peer.get_object_attributes(
                    bucket="photos",
                    key="object",
                    object_attributes=["ObjectParts"],
                    max_parts=1,
                    part_number_marker=10000,
                ).object_parts

                assert count_only is not None
                self.assertEqual(count_only.total_parts_count, 1)
                self.assertEqual(count_only.max_parts, 1)
                self.assertEqual(count_only.part_number_marker, 10000)
                self.assertEqual(count_only.parts, ())
                self.assertFalse(count_only.is_truncated)
                self.assertIsNone(count_only.next_part_number_marker)

                with peer.get_object(bucket="photos", key="object").body as body:
                    self.assertEqual(body.read_all(max_bytes=3), b"abc")

                self.assertEqual(
                    peer.head_object(bucket="photos", key="object").metadata["origin"],
                    "initiation",
                )

                self.assertEqual(
                    peer.get_object_tagging(bucket="photos", key="object")
                    .tags[0]
                    .value,
                    "multipart",
                )

                abandoned = peer.create_multipart_upload(
                    bucket="photos", key="abandoned"
                )

                peer.abort_multipart_upload(
                    bucket="photos", key="abandoned", upload_id=abandoned.upload_id
                )

                self.assertEqual(
                    peer.list_multipart_uploads(bucket="photos").uploads, ()
                )

                with self.assertRaises(TypeError):
                    peer.upload_part(
                        bucket="photos",
                        key="object",
                        upload_id=created.upload_id,
                        part_number=True,
                        body=b"",
                    )

            self.assertEqual(page.parts[0].part_number, 7)
            self.assertEqual(active.uploads[0].key, "object")
            self.assertEqual(copied.checksum_crc64_nvme, "BeXKuz/B+us=")

    def test_attributes_and_part_reads_are_bounded_and_detached(self) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-layout-") as temporary:
            directory = Path(temporary)

            with Store.open(directory=directory) as store:
                store.create_bucket(bucket="photos")

                upload = store.create_multipart_upload(
                    bucket="photos",
                    key="object",
                    checksum_algorithm="SHA256",
                    checksum_type="COMPOSITE",
                )

                part = store.upload_part(
                    bucket="photos",
                    key="object",
                    upload_id=upload.upload_id,
                    part_number=1,
                    body=b"abc",
                )

                store.complete_multipart_upload(
                    bucket="photos",
                    key="object",
                    upload_id=upload.upload_id,
                    parts=[
                        CompletedPart(
                            part_number=1,
                            etag=part.etag,
                            checksum_sha256=part.checksum_sha256,
                        )
                    ],
                )

                output = store.get_object_attributes(
                    bucket="photos",
                    key="object",
                    object_attributes=["ObjectSize", "ETag", "Checksum", "ObjectParts"],
                    max_parts=1,
                )

                self.assertEqual(output.object_size, 3)
                self.assertIsNone(output.storage_class)
                assert output.object_parts is not None
                self.assertEqual(output.object_parts.total_parts_count, 1)

                self.assertEqual(
                    output.object_parts.parts[0].checksum_sha256, part.checksum_sha256
                )

                self.assertEqual(output.object_parts.parts[0].size, 3)
                self.assertFalse(output.object_parts.is_truncated)
                self.assertIsNone(output.object_parts.next_part_number_marker)

                with self.assertRaises(AttributeError):
                    output.object_parts.max_parts = 2  # type: ignore[misc]

                with store.get_object(
                    bucket="photos", key="object", part_number=1
                ).body as body:
                    self.assertEqual(body.read_all(max_bytes=3), b"abc")

                selected = store.head_object(
                    bucket="photos",
                    key="object",
                    part_number=1,
                    checksum_mode="ENABLED",
                )

                self.assertEqual(selected.parts_count, 1)
                self.assertEqual(selected.content_range, "bytes 0-2/3")
                self.assertEqual(selected.checksum_sha256, part.checksum_sha256)

                with self.assertRaises(StorageError) as missing:
                    store.head_object(bucket="photos", key="object", part_number=2)

                self.assertEqual(missing.exception.code, "InvalidPartNumber")

                with self.assertRaises(TypeError):
                    store.head_object(bucket="photos", key="object", part_number=True)

                with self.assertRaises(StorageError) as invalid:
                    store.get_object_attributes(
                        bucket="photos",
                        key="object",
                        object_attributes=["ETag", "ETag"],
                    )

                self.assertEqual(invalid.exception.code, "InvalidArgument")

                with self.assertRaises(StorageError) as invalid:
                    store.get_object_attributes(
                        bucket="photos",
                        key="object",
                        object_attributes=["ETag"],
                        max_parts=0,
                    )

                self.assertEqual(invalid.exception.code, "InvalidArgument")

                for marker in (-1, 10001, 2**64):
                    with self.subTest(marker=marker):
                        with self.assertRaises(StorageError) as invalid:
                            store.get_object_attributes(
                                bucket="photos",
                                key="object",
                                object_attributes=["ObjectParts"],
                                part_number_marker=marker,
                            )

                        self.assertEqual(invalid.exception.code, "InvalidArgument")

                with self.assertRaises(TypeError):
                    store.get_object_attributes(
                        bucket="photos",
                        key="object",
                        object_attributes=["ETag"],
                        part_number_marker=True,
                    )

            self.assertEqual(output.object_parts.parts[0].size, 3)

            with Store.open(directory=directory) as reopened:
                retained = reopened.get_object_attributes(
                    bucket="photos",
                    key="object",
                    object_attributes=["ObjectParts"],
                ).object_parts

                assert retained is not None

                self.assertEqual(
                    retained.parts[0].checksum_sha256, part.checksum_sha256
                )

                self.assertEqual(retained.parts[0].size, 3)

    def test_rejected_copy_and_multipart_calls_preserve_parts_and_object(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="mikrio-multipart-errors-"
        ) as temporary:
            directory = Path(temporary)

            with Store.open(directory=directory) as store:
                store.create_bucket(bucket="photos")

                store.put_object(
                    bucket="photos",
                    key="kept",
                    body=b"original",
                    metadata={"origin": "kept"},
                )

                upload = store.create_multipart_upload(bucket="photos", key="kept")

                part = store.upload_part(
                    bucket="photos",
                    key="kept",
                    upload_id=upload.upload_id,
                    part_number=1,
                    body=b"abc",
                )

                operations: list[tuple[str, Callable[[], object], str, str | None]] = [
                    (
                        "copy",
                        lambda: store.copy_object(
                            bucket="photos",
                            key="kept",
                            copy_source=ObjectRef(bucket="photos", key="kept"),
                            metadata_directive="REPLACE",
                            if_none_match="*",
                        ),
                        "PreconditionFailed",
                        "NotCommitted",
                    ),
                    (
                        "list parts",
                        lambda: store.list_parts(
                            bucket="photos", key="other", upload_id=upload.upload_id
                        ),
                        "NoSuchUpload",
                        None,
                    ),
                    (
                        "list uploads",
                        lambda: store.list_multipart_uploads(bucket="missing"),
                        "NoSuchBucket",
                        None,
                    ),
                    (
                        "complete",
                        lambda: store.complete_multipart_upload(
                            bucket="photos",
                            key="kept",
                            upload_id=upload.upload_id,
                            parts=[
                                CompletedPart(
                                    part_number=1,
                                    etag='"00000000000000000000000000000000"',
                                )
                            ],
                        ),
                        "InvalidPart",
                        "NotCommitted",
                    ),
                    (
                        "abort",
                        lambda: store.abort_multipart_upload(
                            bucket="photos", key="other", upload_id=upload.upload_id
                        ),
                        "NoSuchUpload",
                        "NotCommitted",
                    ),
                    (
                        "copy part",
                        lambda: store.upload_part_copy(
                            bucket="photos",
                            key="kept",
                            upload_id=upload.upload_id,
                            part_number=1,
                            copy_source=ObjectRef(bucket="photos", key="missing"),
                        ),
                        "NoSuchKey",
                        "NotCommitted",
                    ),
                ]

                for name, operation, code, outcome in operations:
                    with self.subTest(operation=name):
                        with self.assertRaises(StorageError) as caught:
                            operation()

                        self.assertEqual(caught.exception.code, code)
                        self.assertEqual(caught.exception.mutation_outcome, outcome)

            with Store.open(directory=directory) as reopened:
                previous = reopened.get_object(bucket="photos", key="kept")

                with previous.body as body:
                    self.assertEqual(previous.metadata, {"origin": "kept"})
                    self.assertEqual(body.read_all(max_bytes=8), b"original")

                parts = reopened.list_parts(
                    bucket="photos", key="kept", upload_id=upload.upload_id
                )

                self.assertEqual(len(parts.parts), 1)
                self.assertEqual(parts.parts[0].etag, part.etag)

                reopened.complete_multipart_upload(
                    bucket="photos",
                    key="kept",
                    upload_id=upload.upload_id,
                    parts=[CompletedPart(part_number=1, etag=part.etag)],
                )

                with reopened.get_object(bucket="photos", key="kept").body as body:
                    self.assertEqual(body.read_all(max_bytes=3), b"abc")
