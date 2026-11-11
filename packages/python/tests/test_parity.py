"""Field and source parity through an ordinary installed wheel."""

from __future__ import annotations

import io
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Iterator
from datetime import UTC, datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from mikrio import StorageError, Store, Tag

# Independent catalog vectors for the ASCII bytes 123456789.
CHECKSUMS = (
    ("CRC32", "checksum_crc32", "y/Q5Jg=="),
    ("CRC32C", "checksum_crc32c", "4waSgw=="),
    ("SHA1", "checksum_sha1", "98O8HYCOBHMq32eZZczDTKeuNEE="),
    ("SHA256", "checksum_sha256", "FeKw08M4keuw8e9gnsQZQgwg4yDOlMZfvIwzEkSOsiU="),
    ("CRC64NVME", "checksum_crc64_nvme", "rosUhgp5mIg="),
)


class ParityTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="mikrio-python-parity-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "store"
        self.store = Store.open(directory=self.root)
        self.addCleanup(self.store.close)
        self.store.create_bucket(bucket="bucket")

    def test_native_sqlite_coexists_in_both_load_orders(self) -> None:
        for order in ("sqlite-first", "engine-first"):
            with self.subTest(order=order):
                result = subprocess.run(
                    [
                        sys.executable,
                        "-I",
                        str(Path(__file__).with_name("coexist.py")),
                        order,
                        str(self.root.parent / order),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=30,
                )

                self.assertTrue(result.stdout.startswith("PASS " + order + " "))

    def test_distinct_images_preserve_acknowledged_writes_and_detached_bodies(
        self,
    ) -> None:
        for order in ("forward", "reverse"):
            with self.subTest(order=order):
                result = subprocess.run(
                    [
                        sys.executable,
                        "-I",
                        str(Path(__file__).with_name("images.py")),
                        order,
                        str(self.root.parent / order),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=30,
                )

                self.assertEqual(result.stdout, f"PASS distinct_images_{order}\n")

    def test_all_integrity_fields_and_representation_values_are_detached(self) -> None:
        for algorithm, field, expected in CHECKSUMS:
            with self.subTest(algorithm=algorithm):
                # Dynamic names exercise every declared checksum field identically.
                options: dict[str, Any] = {
                    "checksum_algorithm": algorithm,
                    field: expected,
                }

                put = self.store.put_object(
                    bucket="bucket",
                    key=algorithm,
                    body=b"123456789",
                    content_length=9,
                    content_md5="JfnnlDI7RTiF9RgfG2JNCw==",
                    content_type="application/example",
                    content_encoding="identity",
                    content_disposition="inline",
                    content_language="en",
                    cache_control="max-age=7",
                    expires=datetime(1, 1, 1, tzinfo=UTC),
                    metadata={"MiXeD": "snow 雪", "empty": ""},
                    tags=[Tag(key="Case", value=""), Tag(key="case", value="one")],
                    **options,
                )

                for _, output_field, _ in CHECKSUMS:
                    self.assertEqual(
                        getattr(put, output_field),
                        expected if field == output_field else None,
                    )

                hidden = self.store.head_object(bucket="bucket", key=algorithm)

                for _, output_field, _ in CHECKSUMS:
                    self.assertIsNone(getattr(hidden, output_field))

                output = self.store.get_object(
                    bucket="bucket", key=algorithm, checksum_mode="ENABLED"
                )

                with output.body as body:
                    self.assertEqual(body.read_all(max_bytes=9), b"123456789")

                self.store.delete_object(bucket="bucket", key=algorithm)
                self.assertEqual(getattr(output, field), expected)
                self.assertEqual(output.checksum_type, "FULL_OBJECT")
                self.assertEqual(output.metadata, {"mixed": "snow 雪", "empty": ""})
                self.assertEqual(output.tag_count, 2)
                self.assertEqual(output.content_type, "application/example")
                self.assertEqual(output.content_encoding, "identity")
                self.assertEqual(output.content_disposition, "inline")
                self.assertEqual(output.content_language, "en")
                self.assertEqual(output.cache_control, "max-age=7")
                self.assertEqual(output.expires, datetime(1, 1, 1, tzinfo=UTC))
                self.assertIsNone(output.parts_count)
                self.assertIsNone(output.content_range)

    def test_empty_optional_strings_and_pre_epoch_rounding_survive_reopen(self) -> None:
        expiry = datetime(1969, 12, 31, 23, 59, 59, 999999, tzinfo=UTC)

        self.store.put_object(
            bucket="bucket",
            key="empty",
            body=b"",
            content_type="",
            content_encoding="",
            content_disposition="",
            content_language="",
            cache_control="",
            expires=expiry,
            tags=[],
            metadata={},
        )

        self.store.close()

        with Store.open(directory=self.root) as reopened:
            output = reopened.head_object(bucket="bucket", key="empty")

            for field in (
                "content_type",
                "content_encoding",
                "content_disposition",
                "content_language",
                "cache_control",
            ):
                self.assertEqual(getattr(output, field), "")

            self.assertEqual(output.expires, expiry.replace(microsecond=999000))
            self.assertEqual(output.tag_count, 0)
            self.assertEqual(output.content_length, 0)

    def test_invalid_conversions_never_pull_or_replace(self) -> None:
        self.store.put_object(bucket="bucket", key="key", body=b"predecessor")

        # These deliberately malformed keyword values exercise runtime conversion.
        invalid: tuple[tuple[dict[str, Any], type[Exception]], ...] = (
            ({"content_length": -1}, StorageError),
            ({"content_length": 2**64}, StorageError),
            ({"content_length": 5 * 1024**3 + 1}, StorageError),
            ({"content_length": True}, TypeError),
            ({"content_length": 1.5}, TypeError),
            ({"content_length": float("nan")}, TypeError),
            ({"content_length": float("inf")}, TypeError),
            ({"metadata": {"A": "a", "a": "b"}}, StorageError),
            ({"metadata": {"surrogate": "\ud800"}}, StorageError),
            ({"metadata": {"key": 1}}, TypeError),
            ({"expected_bucket_owner": ""}, StorageError),
            ({"expected_bucket_owner": "１２３４５６７８９０１２"}, StorageError),
            ({"checksum_algorithm": "sha256"}, StorageError),
            ({"checksum_crc32": "AAAAAB=="}, StorageError),
            (
                {"tags": [Tag(key="same", value="a"), Tag(key="same", value="b")]},
                StorageError,
            ),
            ({"expires": datetime(2020, 1, 1)}, StorageError),
            (
                {"expires": datetime(1, 1, 1, tzinfo=timezone(timedelta(hours=1)))},
                StorageError,
            ),
            (
                {
                    "expires": datetime(
                        9999, 12, 31, 23, 59, tzinfo=timezone(-timedelta(hours=1))
                    )
                },
                StorageError,
            ),
        )

        for options, error_type in invalid:
            with self.subTest(options=options):
                pulls = 0

                def source() -> Iterator[bytes]:
                    nonlocal pulls
                    pulls += 1
                    yield b"unreachable"

                with self.assertRaises(error_type) as rejected:
                    self.store.put_object(
                        bucket="bucket", key="key", body=source(), **options
                    )

                self.assertEqual(pulls, 0)

                if isinstance(rejected.exception, StorageError):
                    self.assertEqual(rejected.exception.code, "InvalidArgument")

                    self.assertEqual(
                        rejected.exception.mutation_outcome, "NotCommitted"
                    )

        with self.store.get_object(bucket="bucket", key="key").body as body:
            self.assertEqual(body.read_all(max_bytes=11), b"predecessor")

    def test_binary_file_and_offset_views_keep_caller_ownership(self) -> None:
        original = bytearray(b"xx\x00\xffpayloadyy")
        view = memoryview(original)[2:-2]
        self.addCleanup(view.release)
        self.store.put_object(bucket="bucket", key="view", body=[b"", view, b""])
        original[:] = b"z" * len(original)

        with self.store.get_object(bucket="bucket", key="view").body as body:
            self.assertEqual(body.read_all(max_bytes=9), b"\x00\xffpayload")

        source = io.BytesIO(b"prefix:binary-file")
        self.addCleanup(source.close)
        source.seek(7)
        self.store.put_object(bucket="bucket", key="file", body=source)
        self.assertFalse(source.closed)
        self.assertEqual(source.tell(), 18)

        with self.store.get_object(bucket="bucket", key="file").body as body:
            self.assertEqual(body.read_chunk(max_bytes=3), b"bin")
            self.assertEqual(body.read_all(max_bytes=8), b"ary-file")
            self.assertIsNone(body.read_chunk(max_bytes=1))
            self.assertIsNone(body.read_chunk(max_bytes=1))

        source.close()

    def test_owner_assertions_execute_through_every_applicable_method(self) -> None:
        # Another process owns the system SQLite connection; this interpreter
        # never loads a second SQLite library beside the native engine.
        owner = subprocess.check_output(
            [
                sys.executable,
                "-I",
                "-c",
                """
import sqlite3
import sys
from contextlib import closing
from pathlib import Path

uri = (Path(sys.argv[1]) / "metadata.sqlite3").as_uri() + "?mode=ro"

with closing(sqlite3.connect(uri, uri=True)) as database:
    owner = database.execute(
        "SELECT owner_account_id FROM store_metadata WHERE singleton_id=1"
    ).fetchone()

    assert owner is not None
    print(owner[0])
""",
                str(self.root),
            ],
            timeout=10,
            text=True,
        ).strip()

        wrong_owner = "000000000001" if owner == "000000000000" else "000000000000"
        self.store.head_bucket(bucket="bucket", expected_bucket_owner=owner)
        self.store.get_bucket_location(bucket="bucket", expected_bucket_owner=owner)

        self.store.put_object(
            bucket="bucket", key="key", body=b"value", expected_bucket_owner=owner
        )

        self.store.head_object(bucket="bucket", key="key", expected_bucket_owner=owner)

        with self.store.get_object(
            bucket="bucket", key="key", expected_bucket_owner=owner
        ).body as body:
            self.assertEqual(body.read_all(max_bytes=5), b"value")

        methods = (
            self.store.head_bucket,
            self.store.get_bucket_location,
            self.store.delete_bucket,
            self.store.head_object,
            self.store.get_object,
            self.store.delete_object,
            self.store.put_object,
        )

        for method in methods:
            with self.subTest(method=method.__name__):
                options: dict[str, Any] = {
                    "bucket": "bucket",
                    "expected_bucket_owner": wrong_owner,
                }

                if "object" in method.__name__:
                    options["key"] = "key"

                if method.__name__ == "put_object":
                    options["body"] = b"replacement"

                with self.assertRaises(StorageError) as rejected:
                    method(**options)

                self.assertEqual(rejected.exception.code, "AccessDenied")

                mutation = method.__name__ in {
                    "put_object",
                    "delete_object",
                    "delete_bucket",
                }

                self.assertEqual(
                    rejected.exception.mutation_outcome,
                    "NotCommitted" if mutation else None,
                )

        self.store.delete_object(
            bucket="bucket", key="key", expected_bucket_owner=owner
        )

        self.store.delete_bucket(bucket="bucket", expected_bucket_owner=owner)

    def test_read_bounds_reject_without_consuming_and_cursors_are_independent(
        self,
    ) -> None:
        self.store.put_object(bucket="bucket", key="key", body=b"abcdef")

        with (
            self.store.get_object(bucket="bucket", key="key").body as first,
            self.store.get_object(bucket="bucket", key="key").body as second,
        ):
            for invalid, error_type in (
                (True, TypeError),
                (-1, StorageError),
                (0, StorageError),
                (2**64, StorageError),
                (1.5, TypeError),
                (float("inf"), TypeError),
                (float("nan"), TypeError),
            ):
                with self.subTest(bound=invalid):
                    with self.assertRaises(error_type) as rejected:
                        first.read_chunk(max_bytes=invalid)  # type: ignore[arg-type]

                    if isinstance(rejected.exception, StorageError):
                        self.assertEqual(rejected.exception.code, "InvalidArgument")

            self.assertEqual(first.read_chunk(max_bytes=2), b"ab")
            self.assertEqual(second.read_chunk(max_bytes=1), b"a")
            self.assertEqual(first.read_all(max_bytes=4), b"cdef")
            self.assertEqual(second.read_all(max_bytes=5), b"bcdef")

    def test_truncated_selected_payload_never_becomes_successful_short_eof(
        self,
    ) -> None:
        self.store.put_object(bucket="bucket", key="key", body=b"x" * 131072)
        output = self.store.get_object(bucket="bucket", key="key")
        self.addCleanup(output.body.close)
        self.assertEqual(output.body.read_chunk(max_bytes=1), b"x")
        (blob,) = self.root.glob("objects/*/*")

        with blob.open("r+b") as damaged:
            damaged.truncate(0)

        with self.assertRaises(StorageError) as failed:
            output.body.read_all(max_bytes=131072)

        self.assertIn(
            failed.exception.code, {"BodyReadFailed", "StoreCorrupt", "IoError"}
        )

        self.assertIsNone(failed.exception.mutation_outcome)
