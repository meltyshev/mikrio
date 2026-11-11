"""Persistent object behavior through the installed package root."""

from __future__ import annotations

import base64
import hashlib
import io
import logging
import tempfile
import threading
import time
import unittest
from collections.abc import Callable, Iterator
from concurrent.futures import ThreadPoolExecutor
from datetime import UTC, datetime, timedelta
from pathlib import Path

from mikrio import (
    CorsRule,
    CreateBucketOutput,
    DeleteBucketOutput,
    DeleteObjectOutput,
    GetObjectOutput,
    ObjectIdentifier,
    PublicAccessBlockConfiguration,
    StorageError,
    Store,
    Tag,
)


class ObjectTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="mikrio-python-objects-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "store"
        self.store = Store.open(directory=self.root)
        self.addCleanup(self.store.close)
        self.store.create_bucket(bucket="bucket")

    def test_every_bucket_owner_option_rejects_before_read_or_mutation(self) -> None:
        store = self.store
        owner = store.get_owner_info().account_id
        wrong_owner = "000000000000" if owner != "000000000000" else "111111111111"

        rules = [
            CorsRule(allowed_origins=["https://app.example"], allowed_methods=["GET"])
        ]

        access_block = PublicAccessBlockConfiguration(
            block_public_acls=True,
            ignore_public_acls=True,
            block_public_policy=True,
            restrict_public_buckets=True,
        )

        policy = (
            '{"Version":"2012-10-17",'
            '"Statement":{"Effect":"Deny",'
            '"Principal":"*",'
            '"Action":"s3:GetObject",'
            '"Resource":"arn:aws:s3:::bucket/*"}}'
        )

        store.put_object(bucket="bucket", key="key", body=b"original")
        store.put_bucket_cors(bucket="bucket", cors_rules=rules)
        store.put_bucket_policy(bucket="bucket", document=policy)

        calls: list[tuple[str, Callable[[str], object]]] = [
            (
                "head_bucket",
                lambda owner: store.head_bucket(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "delete_bucket",
                lambda owner: store.delete_bucket(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "get_bucket_location",
                lambda owner: store.get_bucket_location(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "put_object",
                lambda owner: store.put_object(
                    bucket="bucket",
                    key="key",
                    body=b"changed",
                    expected_bucket_owner=owner,
                ),
            ),
            (
                "get_object",
                lambda owner: store.get_object(
                    bucket="bucket", key="key", expected_bucket_owner=owner
                ),
            ),
            (
                "head_object",
                lambda owner: store.head_object(
                    bucket="bucket", key="key", expected_bucket_owner=owner
                ),
            ),
            (
                "delete_object",
                lambda owner: store.delete_object(
                    bucket="bucket", key="key", expected_bucket_owner=owner
                ),
            ),
            (
                "delete_objects",
                lambda owner: store.delete_objects(
                    bucket="bucket",
                    objects=[ObjectIdentifier(key="key")],
                    expected_bucket_owner=owner,
                ),
            ),
            (
                "list_objects_v2",
                lambda owner: store.list_objects_v2(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "put_bucket_cors",
                lambda owner: store.put_bucket_cors(
                    bucket="bucket", cors_rules=rules, expected_bucket_owner=owner
                ),
            ),
            (
                "get_bucket_cors",
                lambda owner: store.get_bucket_cors(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "delete_bucket_cors",
                lambda owner: store.delete_bucket_cors(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "put_bucket_policy",
                lambda owner: store.put_bucket_policy(
                    bucket="bucket", document=policy, expected_bucket_owner=owner
                ),
            ),
            (
                "get_bucket_policy",
                lambda owner: store.get_bucket_policy(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "delete_bucket_policy",
                lambda owner: store.delete_bucket_policy(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "get_bucket_policy_status",
                lambda owner: store.get_bucket_policy_status(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "put_public_access_block",
                lambda owner: store.put_public_access_block(
                    bucket="bucket",
                    configuration=access_block,
                    expected_bucket_owner=owner,
                ),
            ),
            (
                "get_public_access_block",
                lambda owner: store.get_public_access_block(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "delete_public_access_block",
                lambda owner: store.delete_public_access_block(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "put_bucket_ownership_controls",
                lambda owner: store.put_bucket_ownership_controls(
                    bucket="bucket",
                    object_ownership="BucketOwnerEnforced",
                    expected_bucket_owner=owner,
                ),
            ),
            (
                "get_bucket_ownership_controls",
                lambda owner: store.get_bucket_ownership_controls(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "get_bucket_acl",
                lambda owner: store.get_bucket_acl(
                    bucket="bucket", expected_bucket_owner=owner
                ),
            ),
            (
                "get_object_acl",
                lambda owner: store.get_object_acl(
                    bucket="bucket", key="key", expected_bucket_owner=owner
                ),
            ),
        ]

        self.assertEqual(len(calls), 23)

        for name, call in calls:
            for supplied, code in (
                (wrong_owner, "AccessDenied"),
                ("", "InvalidArgument"),
            ):
                with self.subTest(operation=name, owner=supplied):
                    with self.assertRaises(StorageError) as denied:
                        call(supplied)

                    self.assertEqual(denied.exception.code, code)

                    self.assertEqual(
                        denied.exception.mutation_outcome,
                        "NotCommitted"
                        if name.startswith(("put_", "delete_"))
                        else None,
                    )

                    self.assertIsNone(denied.exception.partial_result)

        # Reopen to check every attempted mutation left its original state intact.
        store.close()

        with Store.open(directory=self.root) as reopened:
            with reopened.get_object(bucket="bucket", key="key").body as body:
                self.assertEqual(body.read_all(max_bytes=8), b"original")

            self.assertEqual(
                reopened.get_bucket_policy(bucket="bucket").document, policy
            )

            self.assertEqual(
                reopened.get_bucket_cors(bucket="bucket").cors_rules[0].allowed_origins,
                ("https://app.example",),
            )

            configuration = reopened.get_public_access_block(
                bucket="bucket"
            ).configuration

            self.assertEqual(
                (
                    configuration.block_public_acls,
                    configuration.ignore_public_acls,
                    configuration.block_public_policy,
                    configuration.restrict_public_buckets,
                ),
                (True, True, True, True),
            )

            self.assertEqual(
                reopened.get_bucket_ownership_controls(
                    bucket="bucket"
                ).object_ownership,
                "BucketOwnerEnforced",
            )

    def test_batch_deletion_preserves_duplicates_quiet_errors_and_frozen_results(
        self,
    ) -> None:
        put = self.store.put_object(bucket="bucket", key="same", body=b"payload")

        output = self.store.delete_objects(
            bucket="bucket",
            objects=[
                ObjectIdentifier(key="same", etag='"wrong"'),
                ObjectIdentifier(key="same", etag=put.etag),
                ObjectIdentifier(key="same", etag=put.etag),
                ObjectIdentifier(key="same"),
            ],
        )

        self.assertEqual([deleted.key for deleted in output.deleted], ["same", "same"])

        self.assertEqual(
            [error.code for error in output.errors], ["PreconditionFailed", "NoSuchKey"]
        )

        self.assertIsInstance(output.deleted, tuple)
        self.assertIsInstance(output.errors, tuple)

        quiet = self.store.delete_objects(
            bucket="bucket",
            objects=(
                ObjectIdentifier(key="missing", etag=etag) for etag in (None, "*")
            ),
            quiet=True,
        )

        self.assertEqual(quiet.deleted, ())
        self.assertEqual(quiet.errors[0].code, "NoSuchKey")

        for objects in ([], [ObjectIdentifier(key="missing")] * 1001):
            with self.subTest(object_count=len(objects)):
                with self.assertRaises(StorageError) as caught:
                    self.store.delete_objects(bucket="bucket", objects=objects)

                self.assertEqual(caught.exception.code, "InvalidArgument")
                self.assertEqual(caught.exception.mutation_outcome, "NotCommitted")
                self.assertIsNone(caught.exception.partial_result)

        with self.assertRaises(TypeError):
            self.store.delete_objects(bucket="bucket", objects=[{"key": "missing"}])  # type: ignore[list-item]

        with self.assertRaises(TypeError):
            self.store.delete_objects(
                bucket="bucket",
                objects=[ObjectIdentifier(key="missing")],
                quiet=1,  # type: ignore[arg-type]
            )

        self.store.close()
        self.assertEqual(output.errors[0].key, "same")

        with self.assertRaises(AttributeError):
            output.errors[0].key = "changed"  # type: ignore[misc]

    def test_batch_close_settles_indexed_progress_after_an_observed_commit(
        self,
    ) -> None:
        self.store.put_object(bucket="bucket", key="first", body=b"payload")

        entries = [ObjectIdentifier(key="first")] + [
            ObjectIdentifier(key="missing")
        ] * 999

        with ThreadPoolExecutor(max_workers=1) as workers:
            pending = workers.submit(
                self.store.delete_objects, bucket="bucket", objects=entries, quiet=True
            )

            deadline = time.monotonic() + 5

            while True:
                try:
                    self.store.head_object(bucket="bucket", key="first")
                except StorageError as missing:
                    self.assertEqual(missing.code, "NoSuchKey")
                    break

                self.assertLess(time.monotonic(), deadline)

            self.store.close()

            with self.assertRaises(StorageError) as caught:
                pending.result(timeout=5)

        error = caught.exception
        self.assertIn(error.code, ("StoreClosing", "OperationCanceled"))
        self.assertEqual(error.mutation_outcome, "Partial")
        progress = error.partial_result
        assert progress is not None
        self.assertEqual(len(progress.items), 1000)
        self.assertEqual(progress.items[0].mutation_outcome, "Committed")
        self.assertEqual(progress.items[-1].mutation_outcome, "NotCommitted")
        self.assertIsNone(progress.items[-1].error)

        self.assertEqual(
            [deletion.index for deletion in progress.items], list(range(1000))
        )

        self.assertIsInstance(progress.items, tuple)

        with self.assertRaises(AttributeError):
            progress.items[0].key = "changed"  # type: ignore[misc]

    def test_bucket_pages_are_live_scoped_and_detached_after_reopen(self) -> None:
        for bucket in ("bucket-a", "bucket-b"):
            self.store.create_bucket(bucket=bucket)

        first = self.store.list_buckets(prefix="bucket", max_buckets=1)
        self.assertEqual([bucket.name for bucket in first.buckets], ["bucket"])
        self.assertIsInstance(first.buckets, tuple)
        self.assertIsInstance(first.buckets[0].creation_date, datetime)
        self.assertEqual(first.buckets[0].bucket_region, "us-east-1")
        self.assertIsNotNone(first.continuation_token)
        self.store.close()

        with Store.open(directory=self.root) as store:
            store.delete_bucket(bucket="bucket-a")
            store.create_bucket(bucket="bucket-c")

            last = store.list_buckets(
                prefix="bucket",
                max_buckets=2,
                continuation_token=first.continuation_token,
            )

            self.assertEqual(
                [bucket.name for bucket in last.buckets], ["bucket-b", "bucket-c"]
            )

            self.assertIsNone(last.continuation_token)

            with self.assertRaises(StorageError) as caught:
                store.list_buckets(continuation_token=first.continuation_token)

            self.assertEqual(caught.exception.code, "InvalidArgument")

            for invalid in (0, -1, 10001, 2**64):
                with self.subTest(max_buckets=invalid):
                    with self.assertRaises(StorageError) as caught:
                        store.list_buckets(max_buckets=invalid)

                    self.assertEqual(caught.exception.code, "InvalidArgument")

            for invalid_type in (True, 1.5, "1"):
                with self.subTest(max_buckets=invalid_type):
                    with self.assertRaises(TypeError):
                        store.list_buckets(max_buckets=invalid_type)  # type: ignore[arg-type]

        self.assertEqual(first.buckets[0].name, "bucket")

        with self.assertRaises(AttributeError):
            first.buckets[0].name = "changed"  # type: ignore[misc]

    def test_ranges_and_all_response_overrides_preserve_stored_metadata(self) -> None:
        self.store.put_object(
            bucket="bucket", key="range", body=b"testcontent", content_type="text/plain"
        )

        expires = datetime(2037, 10, 21, 7, 28, tzinfo=UTC)

        for read in (self.store.get_object, self.store.head_object):
            with self.subTest(operation=read.__name__):
                output = read(
                    bucket="bucket",
                    key="range",
                    range="bytes=4-7",
                    checksum_mode="ENABLED",
                    response_content_type="application/pdf",
                    response_content_encoding="identity",
                    response_content_disposition="attachment",
                    response_content_language="",
                    response_cache_control="no-store",
                    response_expires=expires,
                )

                if isinstance(output, GetObjectOutput):
                    self.addCleanup(output.body.close)

                self.assertEqual(output.content_length, 4)
                self.assertEqual(output.content_range, "bytes 4-7/11")
                self.assertEqual(output.content_type, "application/pdf")
                self.assertEqual(output.content_encoding, "identity")
                self.assertEqual(output.content_disposition, "attachment")
                self.assertEqual(output.content_language, "")
                self.assertEqual(output.cache_control, "no-store")
                self.assertEqual(output.expires, expires)
                self.assertIsNone(output.checksum_crc64_nvme)

                if isinstance(output, GetObjectOutput):
                    with output.body:
                        self.assertEqual(output.body.read_all(max_bytes=4), b"cont")

                with self.assertRaises(StorageError) as caught:
                    read(bucket="bucket", key="range", range="bytes=99-")

                self.assertEqual(caught.exception.code, "InvalidRange")

                for invalid in ("bad\r\nheader", "\ud800"):
                    with self.subTest(response_content_type=invalid):
                        with self.assertRaises(StorageError) as caught:
                            read(
                                bucket="bucket",
                                key="range",
                                response_content_type=invalid,
                            )

                        self.assertEqual(caught.exception.code, "InvalidArgument")

        original = self.store.head_object(
            bucket="bucket", key="range", checksum_mode="ENABLED"
        )

        self.assertEqual(original.content_type, "text/plain")
        self.assertEqual(original.content_length, 11)
        self.assertIsNone(original.content_range)
        self.assertIsNotNone(original.checksum_crc64_nvme)

    def test_listing_pages_reopen_scope_and_detached_values(self) -> None:
        for key in ("a", "b/one", "b/two", "c+雪"):
            self.store.put_object(bucket="bucket", key=key, body=b"abc")

        first = self.store.list_objects_v2(bucket="bucket", delimiter="/", max_keys=2)
        self.assertEqual([summary.key for summary in first.contents], ["a"])

        self.assertEqual(
            [common_prefix.prefix for common_prefix in first.common_prefixes], ["b/"]
        )

        self.assertEqual(first.contents[0].size, 3)
        self.assertIsInstance(first.contents[0].size, int)
        self.assertEqual(first.contents[0].etag, '"900150983cd24fb0d6963f7d28e17f72"')
        self.assertIsInstance(first.contents[0].last_modified, datetime)
        self.assertTrue(first.is_truncated)
        self.assertIsNotNone(first.next_continuation_token)
        self.store.close()

        with Store.open(directory=self.root) as reopened:
            final = reopened.list_objects_v2(
                bucket="bucket",
                delimiter="/",
                max_keys=1,
                continuation_token=first.next_continuation_token,
            )

            self.assertEqual([summary.key for summary in final.contents], ["c+雪"])
            self.assertEqual(final.common_prefixes, ())
            self.assertFalse(final.is_truncated)
            self.assertIsNone(final.next_continuation_token)

            with self.assertRaises(StorageError) as caught:
                reopened.list_objects_v2(
                    bucket="bucket", continuation_token=first.next_continuation_token
                )

            self.assertEqual(caught.exception.code, "InvalidArgument")
            zero = reopened.list_objects_v2(bucket="bucket", max_keys=0)
            self.assertEqual(zero.contents, ())
            self.assertFalse(zero.is_truncated)

            for invalid in (-1, 1001, 2**64):
                with self.subTest(max_keys=invalid):
                    with self.assertRaises(StorageError) as caught:
                        reopened.list_objects_v2(bucket="bucket", max_keys=invalid)

                    self.assertEqual(caught.exception.code, "InvalidArgument")

            with self.assertRaises(TypeError):
                reopened.list_objects_v2(bucket="bucket", max_keys=True)

            with self.assertRaises(StorageError) as caught:
                reopened.list_objects_v2(bucket="bucket", prefix="\ud800")

            self.assertEqual(caught.exception.code, "InvalidArgument")

        self.assertEqual(first.contents[0].key, "a")

    def test_idle_maintenance_logging_is_redacted_reentrant_and_independent_of_close(
        self,
    ) -> None:
        logger = logging.getLogger("mikrio")
        previous = (logger.handlers[:], logger.level, logger.disabled, logger.propagate)
        entered = threading.Event()
        blocked = threading.Event()
        release = threading.Event()
        finished = threading.Event()
        messages: list[str] = []
        failures: list[BaseException] = []
        store = self.store

        def restore_logger() -> None:
            logger.handlers, logger.level, logger.disabled, logger.propagate = previous

        self.addCleanup(restore_logger)
        self.addCleanup(release.set)

        class Observer(logging.Handler):
            def emit(self, record: logging.LogRecord) -> None:
                messages.append(record.getMessage())

                if len(messages) == 1:
                    try:
                        store.head_bucket(bucket="bucket")
                    except Exception as error:
                        failures.append(error)
                    finally:
                        entered.set()

                    raise RuntimeError("observer-secret-canary")

                blocked.set()

                if not release.wait(timeout=20):
                    failures.append(TimeoutError("owned observer was not released"))

                finished.set()

        observer = Observer()
        self.addCleanup(observer.close)
        logger.handlers = [observer]
        logger.setLevel(logging.WARNING)
        logger.disabled = False
        logger.propagate = False
        peer = Store.open(directory=self.root)
        self.addCleanup(peer.close)

        self.store.put_object(
            bucket="bucket", key="secret-key-canary", body=b"secret-body-canary"
        )

        selected = self.store.get_object(bucket="bucket", key="secret-key-canary")
        self.addCleanup(selected.body.close)
        (blob,) = self.root.glob("objects/*/*")
        retained = blob.with_name("secret-path-canary")
        self.store.delete_object(bucket="bucket", key="secret-key-canary")
        blob.rename(retained)

        def restore_blob() -> None:
            if not retained.exists():
                return

            if blob.is_dir():
                blob.rmdir()

            retained.rename(blob)

        self.addCleanup(restore_blob)
        blob.mkdir()
        selected.body.close()

        try:
            # Only maintenance runs until its actual owned-path failure is observed.
            self.assertTrue(entered.wait(timeout=10))
            self.assertTrue(blocked.wait(timeout=10))
            self.assertEqual(failures, [])
            self.assertIn("work=cleanup", messages[0])
            self.assertIn("observer_failures=1", messages[1])

            for message in messages:
                self.assertNotIn("canary", message)
                self.assertNotIn(str(self.root), message)

            self.assertEqual(retained.read_bytes(), b"secret-body-canary")
            blob.rmdir()
            retained.rename(blob)
            deadline = time.monotonic() + 10

            while blob.exists() and time.monotonic() < deadline:
                release.wait(0.02)

            self.assertFalse(
                blob.exists(), "GC must progress while its logger is blocked"
            )

            self.store.close()
            peer.put_object(bucket="bucket", key="peer", body=b"still writable")
            peer.close()
            self.assertFalse(finished.is_set(), "close must not join the host observer")
        finally:
            release.set()

            try:
                if blocked.is_set():
                    self.assertTrue(finished.wait(timeout=10))
            finally:
                restore_logger()
                restore_blob()

        self.assertEqual(failures, [])

    def test_disabled_host_logging_does_not_prevent_maintenance_retry(self) -> None:
        logger = logging.getLogger("mikrio")
        previous = (logger.handlers[:], logger.level, logger.disabled, logger.propagate)
        failed_cleanup = threading.Event()

        def restore_logger() -> None:
            logger.handlers, logger.level, logger.disabled, logger.propagate = previous

        class Observer(logging.Handler):
            def emit(self, record: logging.LogRecord) -> None:
                if "work=cleanup failure=" in record.getMessage():
                    failed_cleanup.set()

        self.addCleanup(restore_logger)
        observer = Observer()
        self.addCleanup(observer.close)
        logger.handlers = [observer]
        logger.setLevel(logging.WARNING)
        logger.disabled = False
        logger.propagate = False
        self.store.put_object(bucket="bucket", key="key", body=b"retained")
        selected = self.store.get_object(bucket="bucket", key="key")
        self.addCleanup(selected.body.close)
        (blob,) = self.root.glob("objects/*/*")
        retained = blob.with_name("unknown-sentinel")
        self.store.delete_object(bucket="bucket", key="key")
        blob.rename(retained)

        def restore_blob() -> None:
            if not retained.exists():
                return

            if blob.is_dir():
                blob.rmdir()

            retained.rename(blob)

        self.addCleanup(restore_blob)
        blob.mkdir()
        selected.body.close()

        try:
            # Observe an actual failed cleanup before disabling delivery for retry.
            self.assertTrue(failed_cleanup.wait(timeout=10))
            logger.disabled = True
            self.assertTrue(blob.is_dir())
            self.assertEqual(retained.read_bytes(), b"retained")
            self.store.put_object(bucket="bucket", key="unrelated", body=b"progress")
            blob.rmdir()
            retained.rename(blob)
            deadline = time.monotonic() + 10

            while blob.exists() and time.monotonic() < deadline:
                threading.Event().wait(0.02)

            self.assertFalse(blob.exists())
        finally:
            restore_logger()
            restore_blob()

    def test_held_deleted_body_reports_bounded_cleanup_sample(self) -> None:
        logger = logging.getLogger("mikrio")
        previous = (logger.handlers[:], logger.level, logger.disabled, logger.propagate)
        observed = threading.Event()
        messages: list[str] = []

        def restore_logger() -> None:
            logger.handlers, logger.level, logger.disabled, logger.propagate = previous

        self.addCleanup(restore_logger)

        class Observer(logging.Handler):
            def emit(self, record: logging.LogRecord) -> None:
                message = record.getMessage()
                messages.append(message)

                if (
                    "bounded cleanup sample" in message
                    and "sampled_blobs=1 observed_file_bytes=19 " in message
                    and "blocked_shard_attempts=0 " not in message
                ):
                    observed.set()

        observer = Observer()
        self.addCleanup(observer.close)
        logger.handlers = [observer]
        logger.setLevel(logging.WARNING)
        logger.disabled = False
        logger.propagate = False

        try:
            self.store.put_object(
                bucket="bucket", key="private-key-canary", body=b"private-body-canary"
            )

            with self.store.get_object(
                bucket="bucket", key="private-key-canary"
            ).body as body:
                self.store.delete_object(bucket="bucket", key="private-key-canary")
                self.assertTrue(observed.wait(timeout=20), "no bounded cleanup sample")

                sample = next(
                    message
                    for message in reversed(messages)
                    if "bounded cleanup sample" in message
                )

                self.assertIn("unobserved_blobs=0 more_pending=false", sample)
                self.assertRegex(sample, r"oldest_sample_blob_age_ms=Some\(\d+\)")
                self.assertEqual(len(list(self.root.glob("objects/*/*"))), 1)
                self.assertEqual(body.read_all(max_bytes=19), b"private-body-canary")

            for message in messages:
                self.assertNotIn("canary", message)
                self.assertNotIn(str(self.root), message)
        finally:
            restore_logger()

    def test_bucket_outputs_regional_repeat_and_retirement(self) -> None:
        self.assertIsInstance(
            self.store.create_bucket(bucket="bucket"), CreateBucketOutput
        )

        self.assertEqual(
            self.store.head_bucket(bucket="bucket").bucket_region, "us-east-1"
        )

        self.assertIsNone(
            self.store.get_bucket_location(bucket="bucket").location_constraint
        )

        self.assertIsInstance(
            self.store.delete_bucket(bucket="bucket"), DeleteBucketOutput
        )

        with self.assertRaises(StorageError) as missing:
            self.store.head_bucket(bucket="bucket")

        self.assertEqual(missing.exception.code, "NoSuchBucket")
        self.store.create_bucket(bucket="bucket")

        with Store.open(
            directory=self.root.parent / "regional", region="eu-west-1"
        ) as regional:
            regional.create_bucket(bucket="bucket")

            with self.assertRaises(StorageError) as repeat:
                regional.create_bucket(bucket="bucket")

            self.assertEqual(repeat.exception.code, "BucketAlreadyOwnedByYou")
            self.assertEqual(repeat.exception.mutation_outcome, "NotCommitted")

    def test_bounded_chunks_metadata_checksums_and_detached_outputs_survive_reopen(
        self,
    ) -> None:
        payload = bytes(range(256)) * 4097
        digest = base64.b64encode(hashlib.sha256(payload).digest()).decode()
        expires = datetime(2028, 2, 3, 4, 5, 6, 123456, tzinfo=UTC)
        source = io.BytesIO(payload)
        self.addCleanup(source.close)

        put = self.store.put_object(
            bucket="bucket",
            key="literal/%2f/雪",
            body=source,
            content_length=len(payload),
            metadata={"MiXeD": "snow 雪"},
            tags=[Tag(key="case", value="one"), Tag(key="Case", value="")],
            content_type="application/example",
            cache_control="max-age=7",
            expires=expires,
            checksum_algorithm="SHA256",
            checksum_sha256=digest,
        )

        self.assertFalse(source.closed)
        self.assertEqual(put.checksum_sha256, digest)

        head = self.store.head_object(
            bucket="bucket", key="literal/%2f/雪", checksum_mode="ENABLED"
        )

        self.assertEqual(head.content_length, len(payload))
        self.assertEqual(head.metadata, {"mixed": "snow 雪"})
        self.assertEqual(head.tag_count, 2)
        self.assertEqual(head.expires, expires.replace(microsecond=123000))
        self.assertEqual(head.last_modified.tzinfo, UTC)
        self.assertEqual(head.last_modified.microsecond % 1000, 0)
        self.assertEqual(head.checksum_sha256, digest)
        self.assertEqual(head.etag, put.etag)

        with self.assertRaises(AttributeError):
            head.content_length = 9  # type: ignore[misc]

        with self.assertRaises(TypeError):
            head.metadata["injected"] = "value"  # type: ignore[index]

        self.store.close()

        with Store.open(directory=self.root) as reopened:
            output = reopened.get_object(bucket="bucket", key="literal/%2f/雪")

            with output.body as body:
                prefix = body.read_chunk(max_bytes=7)
                self.assertEqual(prefix, payload[:7])
                self.assertEqual(body.read_all(max_bytes=len(payload) - 7), payload[7:])
                self.assertIsNone(body.read_chunk(max_bytes=1))
                self.assertEqual(body.read_all(max_bytes=0), b"")

            with self.assertRaises(StorageError) as closed:
                body.read_chunk(max_bytes=1)

            self.assertEqual(closed.exception.code, "BodyClosed")

        self.assertEqual(head.metadata["mixed"], "snow 雪")

    def test_selected_reader_finishes_after_peer_replaces_deletes_and_closes(
        self,
    ) -> None:
        original = b"old payload" * 20000
        self.store.put_object(bucket="bucket", key="key", body=original)

        with Store.open(directory=self.root) as peer:
            selected = self.store.get_object(bucket="bucket", key="key")
            self.addCleanup(selected.body.close)
            prefix = selected.body.read_chunk(max_bytes=3)
            peer.put_object(bucket="bucket", key="key", body=b"replacement")

            self.assertIsInstance(
                peer.delete_object(bucket="bucket", key="key"), DeleteObjectOutput
            )

        self.assertEqual(prefix, original[:3])
        self.assertEqual(selected.body.read_all(max_bytes=len(original)), original[3:])
        selected.body.close()

    def test_full_upload_admission_preserves_upload_and_selected_read_progress(
        self,
    ) -> None:
        self.store.put_object(bucket="bucket", key="selected", body=b"selected bytes")
        selected = self.store.get_object(bucket="bucket", key="selected")
        self.addCleanup(selected.body.close)
        ready = threading.Barrier(33)
        release = threading.Event()
        chunk = b"x" * 65536

        def source() -> Iterator[bytes]:
            ready.wait(timeout=10)

            if not release.wait(timeout=10):
                raise RuntimeError("owned producer was not released")

            yield chunk
            yield chunk

        def upload(index: int) -> str:
            return self.store.put_object(
                bucket="bucket",
                key=f"parallel-{index}",
                body=source(),
                content_length=2 * len(chunk),
            ).etag

        with ThreadPoolExecutor(max_workers=32) as pool:
            pending = [pool.submit(upload, index) for index in range(32)]

            try:
                ready.wait(timeout=10)
                self.assertEqual(selected.body.read_chunk(max_bytes=3), b"sel")

                self.assertEqual(selected.body.read_all(max_bytes=11), b"ected bytes")
            except threading.BrokenBarrierError:
                failures = [future.exception() for future in pending if future.done()]
                self.fail(f"uploads did not all reach their producer: {failures!r}")
            finally:
                release.set()

            digest = hashlib.md5(chunk + chunk, usedforsecurity=False).hexdigest()
            expected = f'"{digest}"'

            self.assertEqual(
                [future.result(timeout=10) for future in pending], [expected] * 32
            )

        for index in range(32):
            self.assertEqual(
                self.store.head_object(
                    bucket="bucket", key=f"parallel-{index}"
                ).content_length,
                2 * len(chunk),
            )

        self.store.put_object(bucket="bucket", key="new", body=b"still usable")

    def test_mutable_large_chunks_are_drained_before_next_pull_and_empty_chunks_are_not_eof(
        self,
    ) -> None:
        chunk = bytearray(b"a" * 400_000)

        def chunks() -> Iterator[bytes | bytearray]:
            yield b""
            yield chunk
            chunk[:] = b"b" * len(chunk)
            yield chunk
            yield b""

        self.store.put_object(bucket="bucket", key="key", body=chunks())

        with self.store.get_object(bucket="bucket", key="key").body as body:
            self.assertEqual(
                body.read_all(max_bytes=800_000), b"a" * 400_000 + b"b" * 400_000
            )

    def test_producer_failure_preserves_original_cause_and_predecessor(self) -> None:
        cause = ValueError("controlled producer failure")
        finalized = False

        def chunks() -> Iterator[bytes]:
            nonlocal finalized

            try:
                yield b"partial"
                raise cause
            finally:
                finalized = True

        self.store.put_object(bucket="bucket", key="key", body=b"original")

        with self.assertRaises(StorageError) as failed:
            self.store.put_object(bucket="bucket", key="key", body=chunks())

        self.assertEqual(failed.exception.code, "BodyReadFailed")
        self.assertEqual(failed.exception.mutation_outcome, "NotCommitted")
        self.assertIs(failed.exception.__cause__, cause)
        self.assertTrue(finalized)

        with self.store.get_object(bucket="bucket", key="key").body as body:
            self.assertEqual(body.read_all(max_bytes=8), b"original")

    def test_validations_do_not_pull_and_domains_have_mutation_outcomes(self) -> None:
        pulled = False

        def chunks() -> Iterator[bytes]:
            nonlocal pulled
            pulled = True
            yield b"unreachable"

        with self.assertRaises(StorageError) as invalid:
            self.store.put_object(
                bucket="bucket",
                key="key",
                body=chunks(),
                content_length=5 * 1024**3 + 1,
            )

        self.assertEqual(invalid.exception.code, "InvalidArgument")
        self.assertEqual(invalid.exception.mutation_outcome, "NotCommitted")
        self.assertFalse(pulled)

        with self.assertRaises(TypeError):
            self.store.put_object(
                bucket="bucket", key="key", body=b"", content_length=True
            )

        with self.assertRaises(StorageError) as surrogate:
            self.store.put_object(bucket="bucket", key="\ud800", body=b"")

        self.assertEqual(surrogate.exception.code, "InvalidArgument")
        self.assertEqual(surrogate.exception.mutation_outcome, "NotCommitted")

        with self.assertRaises(StorageError) as naive:
            self.store.put_object(
                bucket="bucket", key="key", body=b"", expires=datetime(2020, 1, 1)
            )

        self.assertEqual(naive.exception.code, "InvalidArgument")

        with self.assertRaises(StorageError) as duplicate:
            self.store.put_object(
                bucket="bucket", key="key", body=b"", metadata={"A": "one", "a": "two"}
            )

        self.assertEqual(duplicate.exception.code, "InvalidArgument")
        self.assertEqual(duplicate.exception.mutation_outcome, "NotCommitted")

    def test_conditions_and_failed_collection_preserve_expected_cursor_states(
        self,
    ) -> None:
        put = self.store.put_object(
            bucket="bucket", key="key", body=b"payload", if_none_match="*"
        )

        head = self.store.head_object(bucket="bucket", key="key")

        with self.assertRaises(StorageError) as condition:
            self.store.get_object(bucket="bucket", key="key", if_none_match=put.etag)

        self.assertEqual(condition.exception.code, "NotModified")

        with self.assertRaises(StorageError) as failed:
            self.store.delete_object(bucket="bucket", key="key", if_match='"mismatch"')

        self.assertEqual(failed.exception.code, "PreconditionFailed")
        self.assertEqual(failed.exception.mutation_outcome, "NotCommitted")

        # If-Match takes precedence over the conflicting date condition.
        body = self.store.get_object(
            bucket="bucket",
            key="key",
            if_match=put.etag,
            if_unmodified_since=head.last_modified - timedelta(days=1),
        ).body

        self.addCleanup(body.close)

        with self.assertRaises(StorageError) as too_large:
            body.read_all(max_bytes=6)

        self.assertEqual(too_large.exception.code, "BodyTooLarge")

        with self.assertRaises(StorageError) as closed:
            body.read_chunk(max_bytes=1)

        self.assertEqual(closed.exception.code, "BodyClosed")
        body.close()

    def test_store_close_prevents_late_blocked_producer_publication(self) -> None:
        entered = threading.Event()
        release = threading.Event()
        errors: list[BaseException] = []
        finalized = threading.Event()

        def chunks() -> Iterator[bytes]:
            try:
                entered.set()

                if not release.wait(10):
                    raise TimeoutError("test barrier not released")

                yield b"late"
            finally:
                finalized.set()

        def put() -> None:
            try:
                self.store.put_object(bucket="bucket", key="key", body=chunks())
            except BaseException as error:
                errors.append(error)

        worker = threading.Thread(target=put)
        worker.start()

        try:
            self.assertTrue(entered.wait(10))
            self.store.close()
        finally:
            release.set()
            worker.join(10)

        self.assertFalse(worker.is_alive())
        self.assertTrue(finalized.is_set())
        self.assertEqual(len(errors), 1)
        self.assertIsInstance(errors[0], StorageError)

        with Store.open(directory=self.root) as reopened:
            with self.assertRaises(StorageError) as missing:
                reopened.head_object(bucket="bucket", key="key")

            self.assertEqual(missing.exception.code, "NoSuchKey")

    def test_control_flow_exception_is_preserved_and_empty_body_has_eof(self) -> None:
        interruption = KeyboardInterrupt("controlled producer interruption")

        def chunks() -> Iterator[bytes]:
            yield b"partial"
            raise interruption

        with self.assertRaises(KeyboardInterrupt) as caught:
            self.store.put_object(bucket="bucket", key="key", body=chunks())

        self.assertIs(caught.exception, interruption)
        self.store.put_object(bucket="bucket", key="empty", body=b"", content_length=0)

        with self.store.get_object(bucket="bucket", key="empty").body as body:
            self.assertEqual(body.read_all(max_bytes=0), b"")
            self.assertIsNone(body.read_chunk(max_bytes=1))

    def test_cache_instants_round_down_and_retain_the_common_calendar_range(
        self,
    ) -> None:
        instants = [
            datetime(1, 1, 1, tzinfo=UTC),
            datetime(1969, 12, 31, 23, 59, 59, 999999, tzinfo=UTC),
            datetime(9999, 12, 31, 23, 59, 59, 999999, tzinfo=UTC),
        ]

        for instant in instants:
            with self.subTest(instant=instant):
                self.store.put_object(
                    bucket="bucket", key="key", body=b"", expires=instant
                )

                actual = self.store.head_object(bucket="bucket", key="key").expires

                expected = instant.replace(
                    microsecond=instant.microsecond // 1000 * 1000
                )

                self.assertEqual(actual, expected)
