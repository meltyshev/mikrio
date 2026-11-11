"""Tagging ownership and errors through an installed wheel."""

import tempfile
import unittest
from pathlib import Path

from mikrio import StorageError, Store, Tag


class TaggingTests(unittest.TestCase):
    def test_owned_tags_reopen_validation_and_payload_preservation(self) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-tagging-") as temporary:
            directory = Path(temporary)
            path = directory / "store"

            with (
                Store.open(directory=path) as store,
                Store.open(directory=path) as peer,
            ):
                store.create_bucket(bucket="photos")
                store.put_object(bucket="photos", key="key", body=b"payload")
                before = store.head_object(bucket="photos", key="key")

                store.put_object_tagging(
                    bucket="photos",
                    key="key",
                    tags=[
                        Tag(key="日本語", value="𐐀"),
                        Tag(key="Name", value=""),
                        Tag(key="name", value="exact"),
                    ],
                )

                output = peer.get_object_tagging(bucket="photos", key="key")
                expected = [("Name", ""), ("name", "exact"), ("日本語", "𐐀")]
                self.assertIsInstance(output.tags, tuple)

                self.assertEqual(
                    [(tag.key, tag.value) for tag in output.tags], expected
                )

                with self.assertRaises(AttributeError):
                    output.tags[0].value = "changed"  # type: ignore[misc]

                for invalid in [
                    [Tag(key="same", value="one"), Tag(key="same", value="two")],
                    [Tag(key="AWS:reserved", value="")],
                    [Tag(key="key", value="x" * 257)],
                ]:
                    with self.subTest(tags=[(tag.key, tag.value) for tag in invalid]):
                        with self.assertRaises(StorageError) as failed:
                            store.put_object_tagging(
                                bucket="photos", key="key", tags=invalid
                            )

                        self.assertEqual(failed.exception.code, "InvalidArgument")

                        self.assertEqual(
                            failed.exception.mutation_outcome, "NotCommitted"
                        )

                with self.assertRaises(TypeError):
                    store.put_object_tagging(bucket="photos", key="key", tags=None)  # type: ignore[arg-type]

                owner = store.get_owner_info().account_id

                wrong_owner = (
                    "000000000000" if owner != "000000000000" else "111111111111"
                )

                with self.assertRaises(StorageError) as denied:
                    store.get_object_tagging(
                        bucket="photos", key="key", expected_bucket_owner=wrong_owner
                    )

                self.assertEqual(denied.exception.code, "AccessDenied")
                self.assertIsNone(denied.exception.mutation_outcome)

                with self.assertRaises(StorageError) as denied:
                    store.put_object_tagging(
                        bucket="photos",
                        key="key",
                        tags=[],
                        expected_bucket_owner=wrong_owner,
                    )

                self.assertEqual(denied.exception.code, "AccessDenied")
                self.assertEqual(denied.exception.mutation_outcome, "NotCommitted")

                with self.assertRaises(StorageError) as denied:
                    store.delete_object_tagging(
                        bucket="photos", key="key", expected_bucket_owner=wrong_owner
                    )

                self.assertEqual(denied.exception.code, "AccessDenied")
                self.assertEqual(denied.exception.mutation_outcome, "NotCommitted")
                after = store.head_object(bucket="photos", key="key")
                self.assertEqual(after.etag, before.etag)
                self.assertEqual(after.last_modified, before.last_modified)
                self.assertEqual(after.tag_count, 3)

                for bucket, key, code in [
                    ("photos", "missing", "NoSuchKey"),
                    ("missing", "key", "NoSuchBucket"),
                ]:
                    with self.subTest(bucket=bucket, key=key):
                        with self.assertRaises(StorageError) as missing:
                            store.get_object_tagging(bucket=bucket, key=key)

                        self.assertEqual(missing.exception.code, code)
                        self.assertIsNone(missing.exception.mutation_outcome)

                        with self.assertRaises(StorageError) as missing:
                            store.put_object_tagging(bucket=bucket, key=key, tags=[])

                        self.assertEqual(missing.exception.code, code)

                        self.assertEqual(
                            missing.exception.mutation_outcome, "NotCommitted"
                        )

                        with self.assertRaises(StorageError) as missing:
                            store.delete_object_tagging(bucket=bucket, key=key)

                        self.assertEqual(missing.exception.code, code)

                        self.assertEqual(
                            missing.exception.mutation_outcome, "NotCommitted"
                        )

            self.assertEqual([(tag.key, tag.value) for tag in output.tags], expected)

            with Store.open(directory=path) as reopened:
                self.assertEqual(
                    [
                        (tag.key, tag.value)
                        for tag in reopened.get_object_tagging(
                            bucket="photos", key="key"
                        ).tags
                    ],
                    expected,
                )

                reopened.put_object_tagging(bucket="photos", key="key", tags=[])

                self.assertEqual(
                    reopened.get_object_tagging(bucket="photos", key="key").tags, ()
                )

                for _ in range(2):
                    reopened.delete_object_tagging(bucket="photos", key="key")

                with reopened.get_object(bucket="photos", key="key").body as body:
                    self.assertEqual(body.read_all(max_bytes=7), b"payload")

            with self.assertRaises(StorageError) as closed:
                reopened.put_object_tagging(bucket="photos", key="key", tags=[])

            self.assertEqual(closed.exception.code, "StoreClosed")
            self.assertEqual(closed.exception.mutation_outcome, "NotCommitted")
