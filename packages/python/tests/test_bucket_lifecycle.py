"""Installed lifecycle values remain immutable, validated and durable."""

import tempfile
import unittest
from pathlib import Path

from mikrio import (
    AbortIncompleteMultipartUpload,
    LifecycleExpiration,
    LifecycleFilter,
    LifecycleRule,
    StorageError,
    Store,
)


class BucketLifecycleTests(unittest.TestCase):
    def test_atomic_owned_rules_authority_reopen_and_delete(self) -> None:
        rules = [
            LifecycleRule(
                id="retain-😀",
                status="Enabled",
                filter=LifecycleFilter(prefix="drafts/雪"),
                expiration=LifecycleExpiration(days=7),
                abort_incomplete_multipart_upload=AbortIncompleteMultipartUpload(
                    days_after_initiation=1
                ),
            ),
            LifecycleRule(
                status="Disabled",
                filter=LifecycleFilter(),
                expiration=LifecycleExpiration(days=2147483647),
            ),
            LifecycleRule(
                id="",
                status="Disabled",
                filter=LifecycleFilter(prefix=""),
                abort_incomplete_multipart_upload=AbortIncompleteMultipartUpload(
                    days_after_initiation=2
                ),
            ),
        ]

        with tempfile.TemporaryDirectory(prefix="mikrio-retention-") as temporary:
            path = Path(temporary) / "store"

            with Store.open(directory=path) as store:
                store.create_bucket(bucket="photos")

                with self.assertRaises(StorageError) as missing:
                    store.get_bucket_lifecycle_configuration(bucket="photos")

                self.assertEqual(missing.exception.code, "NoSuchLifecycleConfiguration")

                store.put_bucket_lifecycle_configuration(
                    bucket="photos", lifecycle_rules=rules
                )

                output = store.get_bucket_lifecycle_configuration(bucket="photos")
                self.assertIsInstance(output.lifecycle_rules, tuple)
                first = output.lifecycle_rules[0]
                self.assertEqual(first.id, "retain-😀")
                self.assertEqual(first.filter.prefix, "drafts/雪")
                self.assertIsNotNone(first.expiration)
                assert first.expiration is not None
                self.assertEqual(first.expiration.days, 7)
                assert first.abort_incomplete_multipart_upload is not None

                self.assertEqual(
                    first.abort_incomplete_multipart_upload.days_after_initiation, 1
                )

                self.assertIsNone(output.lifecycle_rules[1].filter.prefix)
                self.assertEqual(output.lifecycle_rules[2].filter.prefix, "")
                self.assertTrue(all(rule.id for rule in output.lifecycle_rules))

                with self.assertRaises(AttributeError):
                    first.filter.prefix = "changed"  # type: ignore[misc]

                for invalid in [[], [rules[0], rules[0]], [rules[1]] * 1001]:
                    with self.subTest(rule_count=len(invalid)):
                        with self.assertRaises(StorageError) as failed:
                            store.put_bucket_lifecycle_configuration(
                                bucket="photos", lifecycle_rules=invalid
                            )

                        self.assertEqual(failed.exception.code, "InvalidArgument")

                        self.assertEqual(
                            failed.exception.mutation_outcome, "NotCommitted"
                        )

                owner = store.get_owner_info().account_id

                wrong_owner = (
                    "000000000000" if owner != "000000000000" else "111111111111"
                )

                for bucket, account, code in [
                    ("photos", wrong_owner, "AccessDenied"),
                    ("photos", "", "InvalidArgument"),
                    ("missing", owner, "NoSuchBucket"),
                ]:
                    with self.subTest(bucket=bucket, owner=account):
                        with self.assertRaises(StorageError) as failed:
                            store.put_bucket_lifecycle_configuration(
                                bucket=bucket,
                                lifecycle_rules=rules,
                                expected_bucket_owner=account,
                            )

                        self.assertEqual(failed.exception.code, code)

                        self.assertEqual(
                            failed.exception.mutation_outcome, "NotCommitted"
                        )

                        with self.assertRaises(StorageError) as failed:
                            store.get_bucket_lifecycle_configuration(
                                bucket=bucket, expected_bucket_owner=account
                            )

                        self.assertEqual(failed.exception.code, code)

                        with self.assertRaises(StorageError) as failed:
                            store.delete_bucket_lifecycle(
                                bucket=bucket, expected_bucket_owner=account
                            )

                        self.assertEqual(failed.exception.code, code)

                        self.assertEqual(
                            failed.exception.mutation_outcome, "NotCommitted"
                        )

            with Store.open(directory=path) as reopened:
                self.assertEqual(
                    [
                        rule.id
                        for rule in reopened.get_bucket_lifecycle_configuration(
                            bucket="photos"
                        ).lifecycle_rules
                    ],
                    [rule.id for rule in output.lifecycle_rules],
                )

                for _ in range(2):
                    reopened.delete_bucket_lifecycle(
                        bucket="photos", expected_bucket_owner=owner
                    )

                with self.assertRaises(StorageError) as missing:
                    reopened.get_bucket_lifecycle_configuration(bucket="photos")

                self.assertEqual(missing.exception.code, "NoSuchLifecycleConfiguration")

            self.assertEqual(first.filter.prefix, "drafts/雪")

    def test_nested_values_reject_invalid_types_ranges_and_inactive_rules(self) -> None:
        with self.assertRaises(TypeError):
            LifecycleExpiration(days=True)

        for days in [0, -1, 2147483648, 2**64]:
            with self.subTest(days=days):
                with self.assertRaises(StorageError) as invalid:
                    LifecycleExpiration(days=days)

                self.assertEqual(invalid.exception.code, "InvalidArgument")

            with self.subTest(abort_days=days):
                with self.assertRaises(StorageError) as invalid:
                    AbortIncompleteMultipartUpload(days_after_initiation=days)

                self.assertEqual(invalid.exception.code, "InvalidArgument")

        for prefix in ["a\0", "\ud800", "雪" * 342]:
            with self.subTest(prefix=repr(prefix)):
                with self.assertRaises(StorageError) as invalid:
                    LifecycleFilter(prefix=prefix)

                self.assertEqual(invalid.exception.code, "InvalidArgument")

        with self.assertRaises(StorageError) as invalid:
            LifecycleRule(status="Disabled", filter=LifecycleFilter())

        self.assertEqual(invalid.exception.code, "InvalidArgument")
