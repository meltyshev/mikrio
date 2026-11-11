"""Owned configuration values through the installed public Python package."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from mikrio import PublicAccessBlockConfiguration, StorageError, Store

PUBLIC_POLICY = (
    '{ "Version":"2012-10-17",'
    ' "Statement":{"Effect":"Allow",'
    '"Principal":"*",'
    '"Action":"s3:GetObject",'
    '"Resource":"arn:aws:s3:::photos/*"}}'
)


def access_block_from_bits(bits: int) -> PublicAccessBlockConfiguration:
    return PublicAccessBlockConfiguration(
        block_public_acls=bool(bits & 1),
        ignore_public_acls=bool(bits & 2),
        block_public_policy=bool(bits & 4),
        restrict_public_buckets=bool(bits & 8),
    )


def access_block_bits(configuration: PublicAccessBlockConfiguration) -> int:
    return (
        int(configuration.block_public_acls)
        | int(configuration.ignore_public_acls) << 1
        | int(configuration.block_public_policy) << 2
        | int(configuration.restrict_public_buckets) << 3
    )


class AccessConfigurationTests(unittest.TestCase):
    def test_complete_configuration_acl_and_detached_values_survive_reopen(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-access-") as temporary:
            path = Path(temporary) / "store"

            with (
                Store.open(directory=path) as store,
                Store.open(directory=path) as peer,
            ):
                store.create_bucket(bucket="photos")
                owner = store.get_owner_info()

                self.assertEqual(
                    access_block_bits(
                        peer.get_public_access_block(bucket="photos").configuration
                    ),
                    15,
                )

                with self.assertRaises(StorageError) as rejected:
                    store.put_bucket_policy(bucket="photos", document=PUBLIC_POLICY)

                self.assertEqual(rejected.exception.code, "AccessDenied")
                self.assertEqual(rejected.exception.mutation_outcome, "NotCommitted")
                store.delete_public_access_block(bucket="photos")

                store.put_bucket_policy(
                    bucket="photos",
                    document=PUBLIC_POLICY,
                    expected_bucket_owner=owner.account_id,
                )

                document = peer.get_bucket_policy(bucket="photos")
                self.assertEqual(document.document, PUBLIC_POLICY)
                self.assertNotIn("s3:GetObject", repr(document))

                for combination in range(16):
                    with self.subTest(bits=combination):
                        store.put_public_access_block(
                            bucket="photos",
                            configuration=access_block_from_bits(combination),
                        )

                        result = peer.get_public_access_block(bucket="photos")

                        self.assertEqual(
                            access_block_bits(result.configuration), combination
                        )

                        self.assertTrue(
                            peer.get_bucket_policy_status(bucket="photos").is_public
                        )

                        with self.assertRaises(AttributeError):
                            result.configuration.block_public_acls = False  # type: ignore[misc]

                store.put_bucket_ownership_controls(
                    bucket="photos", object_ownership="BucketOwnerEnforced"
                )

                self.assertEqual(
                    peer.get_bucket_ownership_controls(
                        bucket="photos"
                    ).object_ownership,
                    "BucketOwnerEnforced",
                )

                store.put_object(bucket="photos", key="object", body=b"bytes")
                bucket_acl = peer.get_bucket_acl(bucket="photos")
                object_acl = peer.get_object_acl(bucket="photos", key="object")
                self.assertEqual(bucket_acl.owner.canonical_id, owner.canonical_id)
                self.assertEqual(object_acl.owner.canonical_id, owner.canonical_id)
                self.assertIsInstance(object_acl.grants, tuple)
                self.assertEqual(len(object_acl.grants), 1)

                self.assertEqual(
                    object_acl.grants[0].grantee_canonical_id, owner.canonical_id
                )

                self.assertEqual(object_acl.grants[0].permission, "FULL_CONTROL")

                with self.assertRaises(AttributeError):
                    object_acl.owner.canonical_id = "forged"  # type: ignore[misc]

            self.assertEqual(document.document, PUBLIC_POLICY)
            self.assertEqual(object_acl.owner.canonical_id, owner.canonical_id)

            with Store.open(directory=path) as reopened:
                self.assertEqual(
                    reopened.get_bucket_policy(bucket="photos").document, PUBLIC_POLICY
                )

                self.assertEqual(
                    access_block_bits(
                        reopened.get_public_access_block(bucket="photos").configuration
                    ),
                    15,
                )

                for _ in range(2):
                    reopened.delete_bucket_policy(bucket="photos")
                    reopened.delete_public_access_block(bucket="photos")

                with self.assertRaises(StorageError) as absent_policy:
                    reopened.get_bucket_policy_status(bucket="photos")

                self.assertEqual(absent_policy.exception.code, "NoSuchBucketPolicy")

                with self.assertRaises(StorageError) as absent_configuration:
                    reopened.get_public_access_block(bucket="photos")

                self.assertEqual(
                    absent_configuration.exception.code,
                    "NoSuchPublicAccessBlockConfiguration",
                )

                reopened.put_public_access_block(
                    bucket="photos", configuration=access_block_from_bits(0)
                )

                self.assertEqual(
                    access_block_bits(
                        reopened.get_public_access_block(bucket="photos").configuration
                    ),
                    0,
                )

                reopened.delete_object(bucket="photos", key="object")

                with self.assertRaises(StorageError) as missing:
                    reopened.get_object_acl(bucket="photos", key="object")

                self.assertEqual(missing.exception.code, "NoSuchKey")

    def test_boolean_requiredness_policy_validation_and_closed_mutation(self) -> None:
        with self.assertRaises(TypeError):
            PublicAccessBlockConfiguration(block_public_acls=False)  # type: ignore[call-arg]

        with self.assertRaises(TypeError):
            PublicAccessBlockConfiguration(
                block_public_acls=0,  # type: ignore[arg-type]
                ignore_public_acls=False,
                block_public_policy=False,
                restrict_public_buckets=False,
            )

        with tempfile.TemporaryDirectory(
            prefix="mikrio-access-validation-"
        ) as temporary:
            with Store.open(directory=temporary) as store:
                store.create_bucket(bucket="photos")

                with self.assertRaises(StorageError) as malformed:
                    store.put_bucket_policy(
                        bucket="photos", document='{"Statement":[],"Statement":[]}'
                    )

                self.assertEqual(malformed.exception.code, "InvalidArgument")
                self.assertEqual(malformed.exception.mutation_outcome, "NotCommitted")

                with self.assertRaises(StorageError) as unsupported:
                    store.put_bucket_ownership_controls(
                        bucket="photos",
                        object_ownership="ObjectWriter",  # type: ignore[arg-type]
                    )

                self.assertEqual(unsupported.exception.code, "InvalidArgument")
                self.assertEqual(unsupported.exception.mutation_outcome, "NotCommitted")

                wrong_owner = (
                    "000000000000"
                    if store.get_owner_info().account_id != "000000000000"
                    else "111111111111"
                )

                with self.assertRaises(StorageError) as wrong:
                    store.delete_public_access_block(
                        bucket="photos", expected_bucket_owner=wrong_owner
                    )

                self.assertEqual(wrong.exception.code, "AccessDenied")

                self.assertEqual(
                    access_block_bits(
                        store.get_public_access_block(bucket="photos").configuration
                    ),
                    15,
                )

            with self.assertRaises(StorageError) as closed:
                store.put_public_access_block(
                    bucket="photos", configuration=access_block_from_bits(0)
                )

            self.assertEqual(closed.exception.code, "StoreClosed")
            self.assertEqual(closed.exception.mutation_outcome, "NotCommitted")
