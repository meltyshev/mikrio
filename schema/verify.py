"""Exercise the current SQL design using the production schema.

Run with Python 3.10+ and SQLite 3.37+:
    python3 schema/verify.py

Uses synthetic metadata, memory databases, and temporary WAL files. These tests
do not prove S3 compatibility, OS locking, native bindings, or file durability.
"""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import tempfile
import unittest
from collections.abc import Callable, Iterator
from contextlib import closing, contextmanager
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DDL = (ROOT / "crates/mikrio-core/schema.sql").read_text(encoding="utf-8")
EXPECTED = json.loads((ROOT / "tests/contract/schema.json").read_text())
TABLES = (
    "store_metadata",
    "principals",
    "access_keys",
    "buckets",
    "bucket_lifecycle_rules",
    "blobs",
    "blob_parts",
    "objects",
    "multipart_uploads",
    "multipart_parts",
    "multipart_receipts",
    "maintenance_sweeps",
)
SWEPT_TABLES = ("blobs", "objects", "multipart_uploads", "multipart_receipts")


def new_identity() -> bytes:
    return os.urandom(16)


def new_upload_id(timestamp_ms: int = 0, suffix: bytes | None = None) -> str:
    return (
        "u1-"
        + (timestamp_ms + (1 << 63)).to_bytes(8, "big").hex()
        + (suffix if suffix is not None else new_identity()).hex()
    )


def connect(path: str | Path = ":memory:") -> sqlite3.Connection:
    """Open a configured connection; the caller owns closing it."""
    connection = sqlite3.connect(path, isolation_level=None)

    try:
        connection.execute("PRAGMA foreign_keys = ON")
        connection.execute("PRAGMA trusted_schema = OFF")
        connection.execute("PRAGMA synchronous = FULL")
    except BaseException:
        connection.close()
        raise

    return connection


class SchemaTests(unittest.TestCase):
    def setUp(self) -> None:
        self.db = connect()
        self.addCleanup(self.db.close)
        self.db.executescript("BEGIN IMMEDIATE;\n" + DDL + "\nCOMMIT;")

        self.bucket_id = new_identity()
        self.owner_account_id = "123456789012"
        self.canonical_owner_id = "ab" * 32

        self.insert(
            "buckets",
            bucket_id=self.bucket_id,
            name="uploads",
            block_public_acls=1,
            ignore_public_acls=1,
            block_public_policy=1,
            restrict_public_buckets=1,
            created_at_ms=0,
        )

    def insert(self, table: str, **fields: object) -> sqlite3.Cursor:
        # Identifiers come only from this test source; all values are bound.
        return self.db.execute(
            f"INSERT INTO {table} ({', '.join(fields)}) "
            f"VALUES ({', '.join('?' for _ in fields)})",
            tuple(fields.values()),
        )

    def insert_blob(self, *, reserved: bool = False, **overrides: object) -> bytes:
        blob_id = new_identity()

        fields = dict(
            blob_id=blob_id,
            shard=blob_id[0],
            state="reserved" if reserved else "ready",
            write_kind="put_object",
            created_at_ms=0,
            gc_pending=int(reserved),
            gc_attempt_count=0,
        )

        if reserved:
            fields.update(target_bucket_id=self.bucket_id, target_key=b"key")
        else:
            digest = hashlib.md5(b"abc").digest()

            fields.update(
                byte_length=3,
                etag='"' + digest.hex() + '"',
                md5_digest=digest,
                checksums_json="{}",
            )

        fields.update(overrides)
        self.insert("blobs", **fields)
        return blob_id

    def insert_object(
        self, key: bytes | str = b"key", **overrides: object
    ) -> tuple[bytes, bytes]:
        blob_id = overrides.pop("blob_id", None)

        if blob_id is None:
            blob_id = self.insert_blob()

        generation = new_identity()

        fields = dict(
            bucket_id=self.bucket_id,
            key=key,
            generation=generation,
            blob_id=blob_id,
            user_metadata_json="{}",
            tags_json="{}",
            last_modified_at_ms=0,
            lifecycle_anchor_at_ms=0,
        )

        fields.update(overrides)
        self.insert("objects", **fields)
        assert isinstance(blob_id, bytes)
        return blob_id, generation

    def insert_upload(self, **overrides: object) -> str:
        fields = dict(
            upload_id=new_upload_id(),
            bucket_id=self.bucket_id,
            key=b"mpu",
            initiator_id=self.canonical_owner_id,
            state="open",
            user_metadata_json="{}",
            tags_json="{}",
            initiated_at_ms=0,
        )

        fields.update(overrides)
        self.insert("multipart_uploads", **fields)
        upload_id = fields["upload_id"]
        assert isinstance(upload_id, str)
        return upload_id

    def insert_receipt(self, **overrides: object) -> int:
        fields = dict(
            upload_id=new_upload_id(),
            bucket_id=self.bucket_id,
            key=b"mpu",
            outcome="aborted",
            finished_at_ms=0,
        )

        fields.update(overrides)
        receipt_row_id = self.insert("multipart_receipts", **fields).lastrowid
        assert receipt_row_id is not None
        return receipt_row_id

    @contextmanager
    def rejected_input(self) -> Iterator[None]:
        self.db.execute("SAVEPOINT rejected_input")

        try:
            with self.assertRaises(sqlite3.IntegrityError):
                yield
        finally:
            self.db.execute("ROLLBACK TO rejected_input")
            self.db.execute("RELEASE rejected_input")

    def assert_healthy(self) -> None:
        self.assertEqual(
            self.db.execute("PRAGMA integrity_check").fetchall(), [("ok",)]
        )

        self.assertEqual(self.db.execute("PRAGMA foreign_key_check").fetchall(), [])

    def test_schema_tables_constraints_and_field_order(self) -> None:
        actual = tuple(
            row[0]
            for row in self.db.execute(
                "SELECT name FROM sqlite_schema WHERE type='table' "
                "AND name NOT LIKE 'sqlite_%' ORDER BY rowid"
            )
        )

        self.assertEqual(actual, TABLES)
        table_info = {row[1]: row for row in self.db.execute("PRAGMA table_list")}

        self.assertEqual(
            self.db.execute(
                "SELECT name FROM sqlite_schema WHERE type IN ('view','trigger')"
            ).fetchall(),
            [],
        )

        column_count = 0

        for table in TABLES:
            with self.subTest(table=table):
                self.assertEqual(table_info[table][5], 1, "STRICT required")

                columns = [
                    row[1] for row in self.db.execute(f"PRAGMA table_info({table})")
                ]

                column_count += len(columns)

                self.assertEqual(EXPECTED["tables"][table], columns)

                for foreign_key in self.db.execute(f"PRAGMA foreign_key_list({table})"):
                    self.assertLess(TABLES.index(foreign_key[2]), TABLES.index(table))

                    self.assertEqual(
                        (foreign_key[5], foreign_key[6]), ("RESTRICT", "RESTRICT")
                    )

        self.assertEqual(column_count, 121)

        self.assertEqual(tuple(EXPECTED["tables"]), TABLES)

    def test_schema_matches_frozen_format(self) -> None:
        definitions = self.db.execute(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema "
            "WHERE name NOT GLOB 'sqlite_*' ORDER BY type, name"
        ).fetchall()

        encoded = json.dumps(
            definitions, ensure_ascii=False, separators=(",", ":")
        ).encode("utf-8")

        self.assertEqual(
            hashlib.sha256(encoded).hexdigest(),
            EXPECTED["schema_sha256"],
            f"Storage format {EXPECTED['format_version']} changed; "
            "preserve the released schema and register a migration",
        )

    def test_bootstrap_cardinality_and_sweep_shape(self) -> None:
        values = dict(
            singleton_id=1,
            store_id=new_identity(),
            owner_account_id=self.owner_account_id,
            canonical_owner_id=self.canonical_owner_id,
            format_version=1,
            region="us-east-1",
            shard_count=256,
            shard_algorithm="first_byte",
            continuation_token_key=os.urandom(32),
            created_at_ms=0,
        )

        self.insert("store_metadata", **values)

        with self.rejected_input():
            self.insert("store_metadata", **dict(values, singleton_id=2))

        tasks = (
            ("gc_blobs", range(256)),
            ("expire_objects", [-1]),
            ("abort_uploads", [-1]),
            ("purge_receipts", [-1]),
        )

        for task, shards in tasks:
            for shard in shards:
                self.insert(
                    "maintenance_sweeps",
                    task=task,
                    shard=shard,
                    generation=new_identity(),
                    cursor_row_id=0,
                    upper_row_id=0,
                    started_at_ms=0,
                )

        self.assertEqual(
            self.db.execute("SELECT count(*) FROM maintenance_sweeps").fetchone()[0],
            259,
        )

        with self.rejected_input():
            self.db.execute(
                "UPDATE maintenance_sweeps SET cursor_row_id=1 WHERE task='expire_objects'"
            )

        self.assert_healthy()

    def insert_principal(self, name: str = "app", **overrides: object) -> bytes:
        principal_id = new_identity()

        fields = dict(
            principal_id=principal_id, name=name, is_enabled=1, created_at_ms=0
        )

        fields.update(overrides)
        self.insert("principals", **fields)
        return principal_id

    def insert_access_key(self, principal_id: bytes, **overrides: object) -> str:
        access_key_id = "test" + new_identity().hex()

        fields = dict(
            access_key_id=access_key_id,
            principal_id=principal_id,
            secret_access_key="synthetic-test-secret",
            created_at_ms=0,
        )

        fields.update(overrides)
        self.insert("access_keys", **fields)
        return access_key_id

    def test_owner_identifier_shapes(self) -> None:
        fields = dict(
            singleton_id=1,
            store_id=new_identity(),
            owner_account_id=self.owner_account_id,
            canonical_owner_id=self.canonical_owner_id,
            format_version=1,
            region="us-east-1",
            shard_count=256,
            shard_algorithm="first_byte",
            continuation_token_key=os.urandom(32),
            created_at_ms=0,
        )

        for field, invalid_values in (
            (
                "owner_account_id",
                (
                    "1" * 11,
                    "1" * 13,
                    "a" * 12,
                    "１２３４５６７８９０１２",
                    "1" * 12 + "\0extra",
                    "1" * 11 + "\0",
                ),
            ),
            (
                "canonical_owner_id",
                (
                    "a" * 63,
                    "a" * 65,
                    "A" * 64,
                    "g" * 64,
                    "a" * 64 + "\0extra",
                    "a" * 63 + "\0",
                ),
            ),
        ):
            for invalid in invalid_values:
                with (
                    self.subTest(field=field, invalid=invalid),
                    self.rejected_input(),
                ):
                    self.insert("store_metadata", **dict(fields, **{field: invalid}))

        self.insert("store_metadata", **fields)
        self.assert_healthy()

    def test_principal_key_relations_and_secret_bounds(self) -> None:
        principal_id = self.insert_principal()
        access_key_id = self.insert_access_key(principal_id)

        with self.rejected_input():
            self.insert_principal()

        with self.rejected_input():
            self.insert_principal("disabled-shape", is_enabled=2)

        with self.rejected_input():
            self.insert_access_key(new_identity())

        with self.rejected_input():
            self.insert_access_key(principal_id, access_key_id=access_key_id)

        for secret in (None, "", "x" * 1025, "é" * 513):
            secret_bytes = None if secret is None else len(secret.encode("utf-8"))

            with self.subTest(secret_bytes=secret_bytes), self.rejected_input():
                self.insert_access_key(principal_id, secret_access_key=secret)

        for invalid_access_key_id in (None, "", "x" * 129):
            with (
                self.subTest(access_key_id=invalid_access_key_id),
                self.rejected_input(),
            ):
                self.insert_access_key(
                    principal_id, access_key_id=invalid_access_key_id
                )

        with self.rejected_input():
            self.db.execute(
                "DELETE FROM principals WHERE principal_id=?", (principal_id,)
            )

        self.db.execute("DELETE FROM access_keys WHERE principal_id=?", (principal_id,))
        self.db.execute("DELETE FROM principals WHERE principal_id=?", (principal_id,))
        self.assert_healthy()

    def test_policy_generation_and_utf8_storage_bounds(self) -> None:
        principal_id = self.insert_principal()

        for table, column, target in (
            ("principals", "principal_id", principal_id),
            ("buckets", "bucket_id", self.bucket_id),
        ):
            query = (
                f"UPDATE {table} SET policy_generation=?,policy_json=? WHERE {column}=?"
            )

            for generation, policy in (
                (new_identity(), None),
                (None, "{}"),
                (b"short", "{}"),
                (new_identity(), "x" * 20481),
                (new_identity(), "é" * 10241),
            ):
                policy_bytes = None if policy is None else len(policy.encode("utf-8"))

                with (
                    self.subTest(
                        table=table, generation=generation, policy_bytes=policy_bytes
                    ),
                    self.rejected_input(),
                ):
                    self.db.execute(query, (generation, policy, target))

            # The SQL layer checks representation limits, not the AWS policy grammar.
            self.db.execute(query, (new_identity(), "é" * 10240, target))
            self.db.execute(query, (None, None, target))

        self.assert_healthy()

    def test_public_access_block_presence_and_independent_flags(self) -> None:
        fields = (
            "block_public_acls",
            "ignore_public_acls",
            "block_public_policy",
            "restrict_public_buckets",
        )

        update_query = (
            "UPDATE buckets SET "
            + ",".join(f"{name}=?" for name in fields)
            + " WHERE bucket_id=?"
        )

        read_query = "SELECT " + ",".join(fields) + " FROM buckets WHERE bucket_id=?"

        self.assertEqual(
            self.db.execute(read_query, (self.bucket_id,)).fetchone(), (1, 1, 1, 1)
        )

        for mask in range(16):
            with self.subTest(mask=mask):
                values = tuple((mask >> index) & 1 for index in range(4))
                self.db.execute(update_query, (*values, self.bucket_id))

                self.assertEqual(
                    self.db.execute(read_query, (self.bucket_id,)).fetchone(), values
                )

        for index in range(4):
            for invalid in (None, -1, 2):
                invalid_values: list[int | None] = [0, 0, 0, 0]
                invalid_values[index] = invalid

                with (
                    self.subTest(field=fields[index], invalid=invalid),
                    self.rejected_input(),
                ):
                    self.db.execute(update_query, (*invalid_values, self.bucket_id))

        self.db.execute(update_query, (None, None, None, None, self.bucket_id))

        self.assertEqual(
            self.db.execute(read_query, (self.bucket_id,)).fetchone(), (None,) * 4
        )

        self.assert_healthy()

    def test_lifecycle_rule_representation_and_protocol_bounds(self) -> None:
        fields = dict(
            bucket_id=self.bucket_id,
            rule_index=0,
            rule_id="🦀" * 255,
            is_enabled=0,
            filter_kind="all",
            filter_prefix=b"",
            expiration_days=2147483647,
            abort_incomplete_multipart_days=1,
        )

        overrides: list[dict[str, object]] = [
            {"rule_index": -1},
            {"rule_index": 1000},
            {"rule_id": None},
            {"rule_id": ""},
            {"rule_id": "🦀" * 256},
            {"is_enabled": 2},
            {"filter_kind": "tag"},
            {"filter_prefix": b"nonempty"},
            {"filter_kind": "prefix", "filter_prefix": b"a" * 1025},
            {"expiration_days": 0},
            {"expiration_days": 2147483648},
            {"abort_incomplete_multipart_days": 0},
            {"abort_incomplete_multipart_days": 2147483648},
            {"expiration_days": None, "abort_incomplete_multipart_days": None},
        ]

        for override in overrides:
            with (
                self.subTest(override=override),
                self.assertRaises(sqlite3.IntegrityError),
            ):
                self.insert("bucket_lifecycle_rules", **(fields | override))

        self.insert("bucket_lifecycle_rules", **fields)

        for index, kind in [(1, "prefix"), (999, "legacy_prefix")]:
            self.insert(
                "bucket_lifecycle_rules",
                **(
                    fields
                    | {"rule_index": index, "rule_id": str(index), "filter_kind": kind}
                ),
            )

        with self.assertRaises(sqlite3.IntegrityError):
            self.insert("bucket_lifecycle_rules", **(fields | {"rule_index": 2}))

    def test_deleted_principal_preserves_historical_initiator(self) -> None:
        principal_id = self.insert_principal()
        access_key_id = self.insert_access_key(principal_id)

        principal_arn = (
            f"arn:aws:iam::{self.owner_account_id}:user/{principal_id.hex()}"
        )

        policy = json.dumps(
            {
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": {"AWS": principal_arn},
                        "Action": "s3:GetObject",
                        "Resource": "arn:aws:s3:::uploads/*",
                    }
                ],
            }
        )

        self.db.execute(
            "UPDATE buckets SET policy_generation=?,policy_json=? WHERE bucket_id=?",
            (new_identity(), policy, self.bucket_id),
        )

        upload_id = self.insert_upload(initiator_id=principal_arn)

        self.db.execute("BEGIN IMMEDIATE")

        self.db.execute(
            "DELETE FROM access_keys WHERE access_key_id=?", (access_key_id,)
        )

        self.db.execute("DELETE FROM principals WHERE principal_id=?", (principal_id,))
        self.db.execute("COMMIT")

        replacement_principal_id = self.insert_principal()
        self.assertNotEqual(replacement_principal_id, principal_id)

        self.assertEqual(
            self.db.execute(
                "SELECT initiator_id FROM multipart_uploads WHERE upload_id=?",
                (upload_id,),
            ).fetchone()[0],
            principal_arn,
        )

        self.assertEqual(
            self.db.execute(
                "SELECT policy_json FROM buckets WHERE bucket_id=?", (self.bucket_id,)
            ).fetchone()[0],
            policy,
        )

        # This proves stored identity separation, not runtime policy evaluation.
        self.assert_healthy()

    def test_access_changes_are_atomic_and_fresh_across_wal_connections(self) -> None:
        principal_id = self.insert_principal()
        access_key_id = self.insert_access_key(principal_id)
        old_generation = new_identity()

        policy = json.dumps(
            {
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Deny",
                        "Principal": "*",
                        "Action": "s3:DeleteObject",
                        "Resource": "arn:aws:s3:::uploads/*",
                    }
                ],
            }
        )

        self.db.execute(
            "UPDATE buckets SET policy_generation=?,policy_json=? WHERE bucket_id=?",
            (old_generation, policy, self.bucket_id),
        )

        with tempfile.TemporaryDirectory(prefix="mikrio-access-schema-") as directory:
            path = Path(directory) / "metadata.sqlite3"

            with closing(connect(path)) as writer:
                self.db.backup(writer)

                self.assertEqual(
                    writer.execute("PRAGMA journal_mode=WAL").fetchone()[0], "wal"
                )

                with closing(connect(path)) as reader:
                    query = (
                        "SELECT (SELECT count(*) FROM access_keys WHERE access_key_id=?),"
                        "(SELECT is_enabled FROM principals WHERE principal_id=?),policy_generation "
                        "FROM buckets WHERE bucket_id=?"
                    )

                    params = (access_key_id, principal_id, self.bucket_id)
                    expected_old = (1, 1, old_generation)
                    reader.execute("BEGIN")

                    self.assertEqual(
                        reader.execute(query, params).fetchone(), expected_old
                    )

                    writer.execute("BEGIN IMMEDIATE")

                    writer.execute(
                        "DELETE FROM access_keys WHERE access_key_id=?",
                        (access_key_id,),
                    )

                    writer.execute(
                        "UPDATE principals SET is_enabled=0 WHERE principal_id=?",
                        (principal_id,),
                    )

                    writer.execute(
                        "UPDATE buckets SET policy_generation=NULL,policy_json=NULL WHERE bucket_id=?",
                        (self.bucket_id,),
                    )

                    self.assertEqual(
                        reader.execute(query, params).fetchone(), expected_old
                    )

                    writer.execute("ROLLBACK")

                    self.assertEqual(
                        writer.execute(query, params).fetchone(), expected_old
                    )

                    writer.execute("BEGIN IMMEDIATE")

                    writer.execute(
                        "DELETE FROM access_keys WHERE access_key_id=?",
                        (access_key_id,),
                    )

                    writer.execute(
                        "UPDATE principals SET is_enabled=0 WHERE principal_id=?",
                        (principal_id,),
                    )

                    writer.execute(
                        "UPDATE buckets SET policy_generation=NULL,policy_json=NULL WHERE bucket_id=?",
                        (self.bucket_id,),
                    )

                    writer.execute("COMMIT")

                    self.assertEqual(
                        reader.execute(query, params).fetchone(), expected_old
                    )

                    reader.execute("COMMIT")

                    self.assertEqual(
                        reader.execute(query, params).fetchone(), (0, 0, None)
                    )

                with closing(connect(path)) as reopened:
                    self.assertEqual(
                        reopened.execute(query, params).fetchone(), (0, 0, None)
                    )

    def test_tag_storage_bounds_and_case_preservation(self) -> None:
        # A maximum-length escaped map must fit; this is not a Rust grammar test.
        tags = {("\u754c" * 127 + str(i)): "\u754c" * 256 for i in range(10)}

        encoded = json.dumps(
            tags, ensure_ascii=True, separators=(",", ":"), sort_keys=True
        )

        self.assertLessEqual(len(encoded.encode("utf-8")), 32768)

        for insert_fixture in (self.insert_object, self.insert_upload):
            for invalid in (None, "x" * 32769, "\u754c" * 10923):
                with (
                    self.subTest(
                        insert_fixture=insert_fixture.__name__,
                        size=None if invalid is None else len(invalid),
                    ),
                    self.rejected_input(),
                ):
                    insert_fixture(tags_json=invalid)

        self.insert_object(tags_json=encoded)
        self.insert_upload(tags_json=encoded)

        for table in ("objects", "multipart_uploads"):
            stored = self.db.execute(f"SELECT tags_json FROM {table}").fetchone()[0]
            self.assertEqual(json.loads(stored), tags)

        distinct = {"visibility": "private", "Visibility": "public", "label": ""}
        self.db.execute("UPDATE objects SET tags_json=?", (json.dumps(distinct),))

        self.assertEqual(
            json.loads(self.db.execute("SELECT tags_json FROM objects").fetchone()[0]),
            distinct,
        )

        self.assert_healthy()

    def test_tag_replacement_is_metadata_only_and_rollback_preserves_state(
        self,
    ) -> None:
        blob_id, old_generation = self.insert_object(
            tags_json='{"visibility":"private"}',
            last_modified_at_ms=100,
            lifecycle_anchor_at_ms=90,
        )

        object_query = (
            "SELECT row_id,bucket_id,key,blob_id,content_type,user_metadata_json,"
            "last_modified_at_ms,lifecycle_anchor_at_ms FROM objects WHERE bucket_id=? AND key=?"
        )

        blob_query = (
            "SELECT blob_id,state,byte_length,etag,checksums_json,gc_pending FROM blobs"
        )

        params = (self.bucket_id, b"key")
        before_object = self.db.execute(object_query, params).fetchone()
        before_blobs = self.db.execute(blob_query).fetchall()
        replacement_tags = '{"visibility":"public","label":""}'
        new_generation = new_identity()

        update_query = (
            "UPDATE objects SET tags_json=?,generation=? WHERE bucket_id=? AND key=?"
        )

        self.db.execute("BEGIN IMMEDIATE")
        self.db.execute(update_query, (replacement_tags, new_generation, *params))
        self.db.execute("ROLLBACK")

        self.assertEqual(
            self.db.execute("SELECT tags_json,generation FROM objects").fetchone(),
            ('{"visibility":"private"}', old_generation),
        )

        self.db.execute("BEGIN IMMEDIATE")
        self.db.execute(update_query, (replacement_tags, new_generation, *params))
        self.db.execute("COMMIT")

        self.assertEqual(
            self.db.execute("SELECT tags_json,generation FROM objects").fetchone(),
            (replacement_tags, new_generation),
        )

        self.assertEqual(
            self.db.execute(object_query, params).fetchone(), before_object
        )

        self.assertEqual(self.db.execute(blob_query).fetchall(), before_blobs)

        self.assertEqual(
            self.db.execute(
                "UPDATE objects SET tags_json='{}',generation=? WHERE bucket_id=? AND key=? AND generation=?",
                (new_identity(), *params, old_generation),
            ).rowcount,
            0,
            "a stale object-state fence must lose after tagging",
        )

        self.db.execute(update_query, ("{}", new_identity(), *params))

        self.assertEqual(
            self.db.execute(object_query, params).fetchone(), before_object
        )

        self.db.execute("DELETE FROM objects WHERE bucket_id=? AND key=?", params)

        self.assertEqual(
            self.db.execute(
                update_query, (replacement_tags, new_identity(), *params)
            ).rowcount,
            0,
        )

        self.assertEqual(
            self.db.execute("SELECT count(*) FROM objects").fetchone()[0], 0
        )

        self.assertEqual(self.db.execute(blob_query).fetchall(), before_blobs)
        self.assertEqual(before_object[3], blob_id)
        self.assert_healthy()

    def test_tag_and_object_snapshots_stay_atomic_across_wal_connections(self) -> None:
        old_blob_id, old_generation = self.insert_object(
            tags_json='{"visibility":"public"}'
        )

        replacement_blob_id = self.insert_blob()
        new_generation = new_identity()

        with tempfile.TemporaryDirectory(prefix="mikrio-tag-schema-") as directory:
            path = Path(directory) / "metadata.sqlite3"

            with closing(connect(path)) as writer:
                self.db.backup(writer)

                self.assertEqual(
                    writer.execute("PRAGMA journal_mode=WAL").fetchone()[0], "wal"
                )

                with closing(connect(path)) as reader:
                    query = "SELECT blob_id,generation,tags_json FROM objects WHERE bucket_id=? AND key=?"
                    params = (self.bucket_id, b"key")

                    old_snapshot = (
                        old_blob_id,
                        old_generation,
                        '{"visibility":"public"}',
                    )

                    reader.execute("BEGIN")

                    self.assertEqual(
                        reader.execute(query, params).fetchone(), old_snapshot
                    )

                    writer.execute("BEGIN IMMEDIATE")

                    writer.execute(
                        "UPDATE objects SET blob_id=?,generation=?,tags_json='{}' WHERE bucket_id=? AND key=?",
                        (replacement_blob_id, new_generation, *params),
                    )

                    writer.execute(
                        "UPDATE blobs SET gc_pending=1 WHERE blob_id=?", (old_blob_id,)
                    )

                    self.assertEqual(
                        reader.execute(query, params).fetchone(), old_snapshot
                    )

                    writer.execute("COMMIT")

                    self.assertEqual(
                        reader.execute(query, params).fetchone(), old_snapshot
                    )

                    reader.execute("COMMIT")
                    new_snapshot = (replacement_blob_id, new_generation, "{}")

                    self.assertEqual(
                        reader.execute(query, params).fetchone(), new_snapshot
                    )

                    self.assertEqual(
                        reader.execute("PRAGMA foreign_key_check").fetchall(), []
                    )

                with closing(connect(path)) as reopened:
                    self.assertEqual(
                        reopened.execute(query, params).fetchone(), new_snapshot
                    )

    def test_nonpositive_scan_ids_are_rejected(self) -> None:
        insert_fixtures: tuple[Callable[..., object], ...] = (
            self.insert_blob,
            self.insert_object,
            self.insert_upload,
            self.insert_receipt,
        )

        for table, insert_fixture in zip(SWEPT_TABLES, insert_fixtures, strict=True):
            for row_id in (0, -1):
                with self.subTest(table=table, row_id=row_id), self.rejected_input():
                    insert_fixture(row_id=row_id)

    def test_upload_id_width_is_consistent(self) -> None:
        for invalid in ("", "x" * 50, "x" * 52, "é" * 51):
            with self.subTest(upload_id=invalid):
                with self.rejected_input():
                    self.insert_upload(upload_id=invalid)

                with self.rejected_input():
                    self.insert_receipt(upload_id=invalid)

                with self.rejected_input():
                    self.insert_blob(
                        reserved=True,
                        write_kind="upload_part",
                        target_upload_id=invalid,
                        target_part_number=1,
                    )

        # Rust validates the prefix, hex encoding, and timestamp agreement.
        upload_id = self.insert_upload()

        self.insert(
            "multipart_parts",
            upload_id=upload_id,
            part_number=1,
            generation=new_identity(),
            blob_id=self.insert_blob(),
            last_modified_at_ms=0,
        )

        with self.rejected_input():
            self.db.execute(
                "UPDATE multipart_parts SET upload_id=? WHERE upload_id=?",
                ("x" * 50, upload_id),
            )

        self.assert_healthy()

    def test_key_order_prefixes_and_types(self) -> None:
        keys = [b"a", b"a/", b"a/b", b"a//b", b"A/b", "é/z".encode(), b"a\x00z"]

        for key in keys:
            self.insert_object(key)

        actual = [
            row[0]
            for row in self.db.execute(
                "SELECT key FROM objects WHERE bucket_id=? ORDER BY key",
                (self.bucket_id,),
            )
        ]

        self.assertEqual(actual, sorted(keys))

        prefixed = [
            row[0]
            for row in self.db.execute(
                "SELECT key FROM objects WHERE bucket_id=? AND key>=? AND key<? ORDER BY key",
                (self.bucket_id, b"a/", b"a0"),
            )
        ]

        self.assertEqual(prefixed, sorted(key for key in keys if key.startswith(b"a/")))

        for invalid in (b"", b"x" * 1025, "text-bound-as-text"):
            with (
                self.subTest(key_type=type(invalid).__name__, size=len(invalid)),
                self.rejected_input(),
            ):
                self.insert_object(invalid)

        with self.rejected_input():
            self.insert_object(b"a")

        with self.rejected_input():
            self.insert_object(
                b"large",
                user_metadata_json=json.dumps({"k": "é" * 40000}, ensure_ascii=False),
            )

        self.assert_healthy()

    def test_restrict_relations_and_state_shapes(self) -> None:
        blob_id, _ = self.insert_object()

        with self.rejected_input():
            self.insert_object(b"orphan", blob_id=new_identity())

        with self.rejected_input():
            self.db.execute("DELETE FROM blobs WHERE blob_id=?", (blob_id,))

        with self.rejected_input():
            self.db.execute("DELETE FROM buckets WHERE bucket_id=?", (self.bucket_id,))

        upload_id = self.insert_upload()

        with self.rejected_input():
            self.db.execute(
                "UPDATE multipart_uploads SET state='completing' WHERE upload_id=?",
                (upload_id,),
            )

        with self.rejected_input():
            self.insert_blob(state="unknown")

        with self.rejected_input():
            self.insert_blob(byte_length=-1)

        with self.rejected_input():
            self.insert_blob(md5_digest=b"short")

        self.assert_healthy()

    def test_bucket_namespace_retirement_fences_reuse_and_preserves_children(
        self,
    ) -> None:
        upload_id = self.insert_upload()
        blob_id, _ = self.insert_object()

        retirement_query = (
            "UPDATE buckets SET name=NULL WHERE bucket_id=? AND name IS NOT NULL "
            "AND NOT EXISTS (SELECT 1 FROM objects WHERE bucket_id=? LIMIT 1)"
        )

        self.assertEqual(
            self.db.execute(
                retirement_query, (self.bucket_id, self.bucket_id)
            ).rowcount,
            0,
        )

        self.assertEqual(
            self.db.execute("SELECT name FROM buckets").fetchall(), [("uploads",)]
        )

        self.db.execute("DELETE FROM objects WHERE bucket_id=?", (self.bucket_id,))
        self.db.execute("UPDATE blobs SET gc_pending=1 WHERE blob_id=?", (blob_id,))

        self.db.execute("BEGIN IMMEDIATE")

        self.assertEqual(
            self.db.execute(
                retirement_query, (self.bucket_id, self.bucket_id)
            ).rowcount,
            1,
        )

        self.db.execute("COMMIT")

        replacement_bucket_id = new_identity()

        self.insert(
            "buckets", bucket_id=replacement_bucket_id, name="uploads", created_at_ms=1
        )

        self.assertEqual(
            self.db.execute(
                "SELECT bucket_id FROM buckets WHERE name=?", ("uploads",)
            ).fetchall(),
            [(replacement_bucket_id,)],
        )

        self.assertEqual(
            self.db.execute(
                "SELECT upload_id FROM multipart_uploads WHERE bucket_id=?",
                (self.bucket_id,),
            ).fetchall(),
            [(upload_id,)],
        )

        with self.rejected_input():
            self.db.execute("DELETE FROM buckets WHERE bucket_id=?", (self.bucket_id,))

        # Exercise the live-bucket predicate with an active bucket.
        publication_query = (
            "INSERT INTO objects (bucket_id,key,generation,blob_id,user_metadata_json,"
            "tags_json,last_modified_at_ms,lifecycle_anchor_at_ms) "
            "SELECT bucket_id,?,?,?,?,?,?,? FROM buckets "
            "WHERE bucket_id=? AND name IS NOT NULL"
        )

        self.assertEqual(
            self.db.execute(
                publication_query,
                (b"late", new_identity(), blob_id, "{}", "{}", 0, 0, self.bucket_id),
            ).rowcount,
            0,
        )

        self.assertEqual(self.db.execute("SELECT key FROM objects").fetchall(), [])

        query = "SELECT bucket_id FROM buckets WHERE name IS NULL AND bucket_id>? ORDER BY bucket_id LIMIT ?"

        self.assertEqual(
            self.db.execute(query, (b"", 64)).fetchall(), [(self.bucket_id,)]
        )

        plan = str(self.db.execute("EXPLAIN QUERY PLAN " + query, (b"", 64)).fetchall())
        self.assertIn("buckets_retiring_idx", plan)
        self.assertNotIn("TEMP B-TREE", plan)
        self.assert_healthy()

    def test_bucket_retirement_resumes_bounded_units_after_rollback_and_reopen(
        self,
    ) -> None:
        upload_ids = [
            self.insert_upload(key=f"upload-{index}".encode()) for index in range(3)
        ]

        part_blob_ids = []

        for upload_id in upload_ids:
            for part_number in (1, 2):
                blob_id = self.insert_blob(write_kind="upload_part")
                part_blob_ids.append(blob_id)

                self.insert(
                    "multipart_parts",
                    upload_id=upload_id,
                    part_number=part_number,
                    generation=new_identity(),
                    blob_id=blob_id,
                    last_modified_at_ms=0,
                )

        completion_blob_id = self.insert_blob(
            reserved=True,
            write_kind="complete_multipart_upload",
            target_upload_id=upload_ids[0],
        )

        completion_claim_id, completion_parts_digest = new_identity(), bytes(32)

        self.db.execute(
            "UPDATE multipart_uploads SET state='completing',completion_claim_id=?,"
            "completion_blob_id=?,completion_parts_digest=? WHERE upload_id=?",
            (
                completion_claim_id,
                completion_blob_id,
                completion_parts_digest,
                upload_ids[0],
            ),
        )

        for index in range(257):
            self.insert(
                "bucket_lifecycle_rules",
                bucket_id=self.bucket_id,
                rule_index=index,
                rule_id=f"rule-{index}",
                is_enabled=1,
                filter_kind="all",
                filter_prefix=b"",
                expiration_days=1,
            )

        self.db.execute(
            "UPDATE buckets SET name=NULL WHERE bucket_id=?", (self.bucket_id,)
        )

        replacement_bucket_id = new_identity()

        self.insert(
            "buckets", bucket_id=replacement_bucket_id, name="uploads", created_at_ms=1
        )

        replacement_blob_id, _ = self.insert_object(
            b"new", bucket_id=replacement_bucket_id
        )

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "metadata.sqlite3"

            with closing(connect(path)) as db:
                self.db.backup(db)

                self.assertEqual(
                    db.execute("PRAGMA journal_mode=WAL").fetchone()[0], "wal"
                )

                db.execute("BEGIN IMMEDIATE")
                self.retire_bucket_metadata_unit(db, self.bucket_id)
                db.execute("ROLLBACK")

                self.assertEqual(
                    db.execute("SELECT count(*) FROM multipart_parts").fetchone()[0], 6
                )

                self.assertEqual(
                    db.execute("SELECT count(*) FROM multipart_receipts").fetchone()[0],
                    0,
                )

                self.assertEqual(
                    db.execute(
                        "SELECT count(*) FROM bucket_lifecycle_rules"
                    ).fetchone()[0],
                    257,
                )

                self.assertEqual(
                    db.execute("SELECT sum(gc_pending) FROM blobs").fetchone()[0], 1
                )

            # Each unit resumes after reopen with no saved child cursor.
            for remaining_upload_count, remaining_rule_count in (
                (2, 1),
                (1, 0),
                (0, 0),
            ):
                with closing(connect(path)) as db:
                    db.execute("BEGIN IMMEDIATE")
                    self.retire_bucket_metadata_unit(db, self.bucket_id)
                    db.execute("COMMIT")

                    self.assertEqual(
                        db.execute("SELECT count(*) FROM multipart_uploads").fetchone()[
                            0
                        ],
                        remaining_upload_count,
                    )

                    self.assertEqual(
                        db.execute(
                            "SELECT count(*) FROM bucket_lifecycle_rules"
                        ).fetchone()[0],
                        remaining_rule_count,
                    )

                    self.assertEqual(
                        db.execute("PRAGMA foreign_key_check").fetchall(), []
                    )

            with closing(connect(path)) as db:
                self.assertEqual(
                    db.execute("SELECT bucket_id,name FROM buckets").fetchall(),
                    [(replacement_bucket_id, "uploads")],
                )

                self.assertEqual(
                    db.execute("SELECT blob_id FROM objects").fetchall(),
                    [(replacement_blob_id,)],
                )

                self.assertEqual(
                    db.execute(
                        "SELECT gc_pending FROM blobs WHERE blob_id=?",
                        (replacement_blob_id,),
                    ).fetchone(),
                    (0,),
                )

                for blob_id in [*part_blob_ids, completion_blob_id]:
                    self.assertEqual(
                        db.execute(
                            "SELECT gc_pending FROM blobs WHERE blob_id=?", (blob_id,)
                        ).fetchone(),
                        (1,),
                    )

                self.assertEqual(
                    db.execute(
                        "SELECT outcome,completion_claim_id,completion_blob_id,completion_parts_digest "
                        "FROM multipart_receipts WHERE upload_id=?",
                        (upload_ids[0],),
                    ).fetchone(),
                    (
                        "aborted",
                        completion_claim_id,
                        completion_blob_id,
                        completion_parts_digest,
                    ),
                )

                self.assertEqual(
                    db.execute("PRAGMA integrity_check").fetchall(), [("ok",)]
                )

    @staticmethod
    def retire_bucket_metadata_unit(db: sqlite3.Connection, bucket_id: bytes) -> None:
        """Exercise the documented SQL recipe only; no native maintenance or file GC."""
        retired_bucket = db.execute(
            "SELECT 1 FROM buckets WHERE bucket_id=? AND name IS NULL", (bucket_id,)
        ).fetchone()

        if retired_bucket is None:
            raise AssertionError("retirement requires the old hidden parent")

        db.execute(
            "DELETE FROM bucket_lifecycle_rules WHERE bucket_id=? AND rule_index IN "
            "(SELECT rule_index FROM bucket_lifecycle_rules WHERE bucket_id=? ORDER BY rule_index LIMIT 256)",
            (bucket_id, bucket_id),
        )

        selected = db.execute(
            "SELECT upload_id FROM multipart_uploads WHERE bucket_id=? ORDER BY key,upload_id LIMIT 1",
            (bucket_id,),
        ).fetchone()

        if selected is not None:
            upload_id = selected[0]

            db.execute(
                "UPDATE blobs SET gc_pending=1 WHERE blob_id IN "
                "(SELECT blob_id FROM multipart_parts WHERE upload_id=? UNION ALL "
                "SELECT completion_blob_id FROM multipart_uploads WHERE upload_id=?)",
                (upload_id, upload_id),
            )

            db.execute("DELETE FROM multipart_parts WHERE upload_id=?", (upload_id,))

            db.execute(
                "INSERT INTO multipart_receipts (upload_id,bucket_id,key,outcome,completion_claim_id,"
                "completion_blob_id,completion_parts_digest,finished_at_ms) "
                "SELECT upload_id,bucket_id,key,'aborted',completion_claim_id,completion_blob_id,"
                "completion_parts_digest,0 FROM multipart_uploads WHERE upload_id=?",
                (upload_id,),
            )

            db.execute("DELETE FROM multipart_uploads WHERE upload_id=?", (upload_id,))

        db.execute(
            "DELETE FROM buckets WHERE bucket_id=? AND name IS NULL "
            "AND NOT EXISTS (SELECT 1 FROM bucket_lifecycle_rules WHERE bucket_id=?) "
            "AND NOT EXISTS (SELECT 1 FROM multipart_uploads WHERE bucket_id=?) "
            "AND NOT EXISTS (SELECT 1 FROM objects WHERE bucket_id=?)",
            (bucket_id,) * 4,
        )

    def test_statement_error_and_lost_fence_require_full_rollback(self) -> None:
        old_blob_id, generation = self.insert_object()
        candidate_blob_id = self.insert_blob(reserved=True)

        self.db.execute("BEGIN IMMEDIATE")

        self.db.execute(
            "UPDATE blobs SET state='ready',target_bucket_id=NULL,target_key=NULL,"
            "byte_length=3,etag=?,md5_digest=?,checksums_json='{}',gc_pending=0 WHERE blob_id=?",
            ('"new"', bytes(16), candidate_blob_id),
        )

        self.db.execute("UPDATE blobs SET gc_pending=1 WHERE blob_id=?", (old_blob_id,))

        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute(
                "UPDATE blobs SET byte_length=-1 WHERE blob_id=?", (candidate_blob_id,)
            )

        self.assertTrue(self.db.in_transaction)

        self.assertEqual(
            self.db.execute(
                "SELECT state FROM blobs WHERE blob_id=?", (candidate_blob_id,)
            ).fetchone()[0],
            "ready",
        )

        lost_fence = self.db.execute(
            "UPDATE objects SET blob_id=? WHERE bucket_id=? AND key=? AND generation=?",
            (candidate_blob_id, self.bucket_id, b"key", new_identity()),
        )

        self.assertEqual(lost_fence.rowcount, 0)
        self.db.execute("ROLLBACK")

        self.assertEqual(
            self.db.execute(
                "SELECT state FROM blobs WHERE blob_id=?", (candidate_blob_id,)
            ).fetchone()[0],
            "reserved",
        )

        self.assertEqual(
            self.db.execute("SELECT blob_id,generation FROM objects").fetchone(),
            (old_blob_id, generation),
        )

        self.assertEqual(
            self.db.execute(
                "SELECT gc_pending FROM blobs WHERE blob_id=?", (old_blob_id,)
            ).fetchone()[0],
            0,
        )

        self.assert_healthy()

    def test_listing_query_preserves_corrupt_reference_for_detection(self) -> None:
        blob_id, _ = self.insert_object()

        # Deliberately inject corruption; this is not an allowed engine operation.
        self.db.execute("PRAGMA foreign_keys=OFF")
        self.db.execute("DELETE FROM blobs WHERE blob_id=?", (blob_id,))
        self.db.execute("PRAGMA foreign_keys=ON")

        query = EXPECTED["listing_query"]

        rows = self.db.execute(
            query,
            dict(
                bucket_id=self.bucket_id,
                prefix_lower=b"",
                prefix_upper=b"z",
                after_key=b"",
                page_limit=10,
            ),
        ).fetchall()

        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0][:2], (b"key", blob_id))
        self.assertIsNone(rows[0][2], "missing payload must remain detectable")

        violations = self.db.execute("PRAGMA foreign_key_check").fetchall()

        self.assertEqual(
            [(row[0], row[2]) for row in violations], [("objects", "blobs")]
        )

    def test_sweep_bounds_empty_source_and_stale_cursor(self) -> None:
        first_receipt_row_id = self.insert_receipt()

        upper_row_id = self.db.execute(
            "SELECT MAX(row_id) FROM multipart_receipts"
        ).fetchone()[0]

        self.db.execute("DELETE FROM multipart_receipts")
        second_receipt_row_id = self.insert_receipt()
        self.assertGreater(second_receipt_row_id, first_receipt_row_id)

        self.assertEqual(
            self.db.execute(
                "SELECT row_id FROM multipart_receipts WHERE row_id>0 AND row_id<=?",
                (upper_row_id,),
            ).fetchall(),
            [],
        )

        sweep_generation = new_identity()

        self.insert(
            "maintenance_sweeps",
            task="purge_receipts",
            shard=-1,
            generation=sweep_generation,
            cursor_row_id=0,
            upper_row_id=upper_row_id,
            started_at_ms=0,
        )

        query = (
            "UPDATE maintenance_sweeps SET cursor_row_id=? WHERE task='purge_receipts' "
            "AND shard=-1 AND generation=? AND cursor_row_id=0"
        )

        self.assertEqual(
            self.db.execute(query, (upper_row_id, sweep_generation)).rowcount, 1
        )

        self.assertEqual(
            self.db.execute(query, (upper_row_id, sweep_generation)).rowcount, 0
        )

        self.db.execute("DELETE FROM multipart_receipts")

        self.assertEqual(
            self.db.execute(
                "SELECT COALESCE(MAX(row_id),0) FROM multipart_receipts"
            ).fetchone()[0],
            0,
        )

        self.assert_healthy()

    def test_multipart_order_and_terminal_transaction(self) -> None:
        for i, timestamp_ms in enumerate((1000, -1, 0, 1000)):
            self.insert_upload(
                upload_id=new_upload_id(timestamp_ms, bytes([i]) * 16),
                initiated_at_ms=timestamp_ms,
            )

        timestamps_ms = [
            row[0]
            for row in self.db.execute(
                "SELECT initiated_at_ms FROM multipart_uploads WHERE bucket_id=? ORDER BY key,upload_id",
                (self.bucket_id,),
            )
        ]

        self.assertEqual(timestamps_ms, [-1, 0, 1000, 1000])

        upload_tags = '{"visibility":"private","label":"multipart"}'
        upload_id = self.insert_upload(key=b"complete", tags_json=upload_tags)
        part_blob_ids = [self.insert_blob(write_kind="upload_part") for _ in range(3)]

        for part_number, blob_id in enumerate(part_blob_ids, 1):
            self.insert(
                "multipart_parts",
                upload_id=upload_id,
                part_number=part_number,
                generation=new_identity(),
                blob_id=blob_id,
                last_modified_at_ms=0,
            )

        completion_blob_id = self.insert_blob(
            reserved=True,
            write_kind="complete_multipart_upload",
            target_upload_id=upload_id,
        )

        completion_claim_id, completion_parts_digest = new_identity(), bytes(32)

        self.db.execute(
            "UPDATE multipart_uploads SET state='completing',completion_claim_id=?,"
            "completion_blob_id=?,completion_parts_digest=? WHERE upload_id=?",
            (
                completion_claim_id,
                completion_blob_id,
                completion_parts_digest,
                upload_id,
            ),
        )

        with self.rejected_input():
            self.db.execute("DELETE FROM blobs WHERE blob_id=?", (completion_blob_id,))

        self.db.execute("BEGIN IMMEDIATE")
        etag = '"' + hashlib.md5(hashlib.md5(b"abc").digest() * 2).hexdigest() + '-2"'

        self.db.execute(
            "UPDATE blobs SET state='ready',target_bucket_id=NULL,target_key=NULL,target_upload_id=NULL,"
            "byte_length=6,part_count=2,etag=?,md5_digest=?,checksums_json='{}',gc_pending=0 WHERE blob_id=?",
            (etag, hashlib.md5(b"abcabc").digest(), completion_blob_id),
        )

        for part_index, (part_number, byte_offset) in enumerate(
            ((1, 0), (3, 3)), start=1
        ):
            self.insert(
                "blob_parts",
                blob_id=completion_blob_id,
                part_number=part_number,
                part_index=part_index,
                byte_offset=byte_offset,
                byte_length=3,
                checksums_json="{}",
            )

        selected_tags = self.db.execute(
            "SELECT tags_json FROM multipart_uploads WHERE upload_id=?", (upload_id,)
        ).fetchone()[0]

        _, generation = self.insert_object(
            b"complete", blob_id=completion_blob_id, tags_json=selected_tags
        )

        self.db.execute(
            "UPDATE blobs SET gc_pending=1 WHERE blob_id IN "
            "(SELECT blob_id FROM multipart_parts WHERE upload_id=?)",
            (upload_id,),
        )

        self.db.execute("DELETE FROM multipart_parts WHERE upload_id=?", (upload_id,))

        self.insert_receipt(
            upload_id=upload_id,
            key=b"complete",
            outcome="completed",
            completion_claim_id=completion_claim_id,
            completion_blob_id=completion_blob_id,
            completion_parts_digest=completion_parts_digest,
            result_json=json.dumps(
                dict(
                    object_generation=generation.hex(),
                    byte_length=6,
                    etag=etag,
                    checksums={},
                )
            ),
        )

        self.db.execute(
            "DELETE FROM multipart_uploads WHERE upload_id=? AND completion_claim_id=?",
            (upload_id, completion_claim_id),
        )

        self.db.execute("COMMIT")

        self.assertEqual(
            self.db.execute(
                "SELECT tags_json FROM objects WHERE key=?", (b"complete",)
            ).fetchone()[0],
            upload_tags,
        )

        self.assertEqual(
            self.db.execute(
                "SELECT count(*) FROM multipart_parts WHERE upload_id=?", (upload_id,)
            ).fetchone()[0],
            0,
        )

        for blob_id in part_blob_ids:
            self.assertEqual(
                self.db.execute(
                    "SELECT gc_pending FROM blobs WHERE blob_id=?", (blob_id,)
                ).fetchone()[0],
                1,
            )

            self.db.execute("DELETE FROM blobs WHERE blob_id=?", (blob_id,))

        self.assertEqual(
            self.db.execute(
                "SELECT part_number,byte_offset,byte_length FROM blob_parts WHERE blob_id=? ORDER BY part_number",
                (completion_blob_id,),
            ).fetchall(),
            [(1, 0, 3), (3, 3, 3)],
        )

        # Metadata-only simulation: receipt must not be a payload FK after overwrite.
        replacement_blob_id = self.insert_blob()

        self.db.execute(
            "UPDATE objects SET blob_id=?,generation=?,tags_json='{}' WHERE bucket_id=? AND key=?",
            (replacement_blob_id, new_identity(), self.bucket_id, b"complete"),
        )

        self.assertEqual(
            self.db.execute(
                "SELECT tags_json FROM objects WHERE key=?", (b"complete",)
            ).fetchone()[0],
            "{}",
        )

        with self.rejected_input():
            self.db.execute("DELETE FROM blobs WHERE blob_id=?", (completion_blob_id,))

        self.db.execute("DELETE FROM blob_parts WHERE blob_id=?", (completion_blob_id,))
        self.db.execute("DELETE FROM blobs WHERE blob_id=?", (completion_blob_id,))

        self.assertEqual(
            self.db.execute(
                "SELECT completion_blob_id FROM multipart_receipts WHERE upload_id=?",
                (upload_id,),
            ).fetchone()[0],
            completion_blob_id,
        )

        self.assert_healthy()

    def test_completed_part_constraints_and_count_shape(self) -> None:
        with self.rejected_input():
            self.insert_blob(part_count=1)

        with self.rejected_input():
            self.insert_blob(reserved=True, part_count=1)

        with self.rejected_input():
            self.insert_blob(write_kind="complete_multipart_upload")

        with self.rejected_input():
            self.insert_blob(write_kind="complete_multipart_upload", part_count=0)

        completion_blob_id = self.insert_blob(
            write_kind="complete_multipart_upload", part_count=1
        )

        valid = dict(
            blob_id=completion_blob_id,
            part_number=1,
            part_index=1,
            byte_offset=0,
            byte_length=3,
            checksums_json="{}",
        )

        with self.rejected_input():
            self.insert(
                "blob_parts",
                **{
                    field: value
                    for field, value in valid.items()
                    if field != "part_index"
                },
            )

        for case, field, value in (
            ("missing blob", "blob_id", new_identity()),
            ("zero part number", "part_number", 0),
            ("zero part index", "part_index", 0),
            ("part index above maximum", "part_index", 10001),
            ("null part index", "part_index", None),
            ("text part index", "part_index", "invalid"),
            ("negative offset", "byte_offset", -1),
            ("negative length", "byte_length", -1),
            ("null checksums", "checksums_json", None),
            ("checksums above byte limit", "checksums_json", "é" * 2049),
        ):
            with self.subTest(case=case), self.rejected_input():
                self.insert("blob_parts", **dict(valid, **{field: value}))

        self.insert("blob_parts", **valid)

        with self.rejected_input():
            self.insert("blob_parts", **valid)

        with self.rejected_input():
            self.insert("blob_parts", **dict(valid, part_number=2))

        with self.rejected_input():
            self.db.execute("DELETE FROM blobs WHERE blob_id=?", (completion_blob_id,))

        self.assert_healthy()

    def test_completed_part_publication_rolls_back_as_one_unit(self) -> None:
        old_blob_id, generation = self.insert_object()

        completion_blob_id = self.insert_blob(
            reserved=True,
            write_kind="complete_multipart_upload",
            target_upload_id=new_upload_id(),
        )

        self.db.execute("BEGIN IMMEDIATE")

        self.db.execute(
            "UPDATE blobs SET state='ready',target_bucket_id=NULL,target_key=NULL,target_upload_id=NULL,"
            "byte_length=3,part_count=1,etag='etag',md5_digest=?,checksums_json='{}',gc_pending=0 WHERE blob_id=?",
            (bytes(16), completion_blob_id),
        )

        self.insert(
            "blob_parts",
            blob_id=completion_blob_id,
            part_number=1,
            part_index=1,
            byte_offset=0,
            byte_length=3,
            checksums_json="{}",
        )

        self.db.execute(
            "UPDATE objects SET blob_id=?,generation=? WHERE key=?",
            (completion_blob_id, new_identity(), b"key"),
        )

        with self.assertRaises(sqlite3.IntegrityError):
            self.insert(
                "blob_parts",
                blob_id=completion_blob_id,
                part_number=1,
                part_index=1,
                byte_offset=0,
                byte_length=3,
                checksums_json="{}",
            )

        # A failed statement does not roll back earlier statements; the owner must do so.
        self.db.execute("ROLLBACK")

        self.assertEqual(
            self.db.execute(
                "SELECT blob_id,generation FROM objects WHERE key=?", (b"key",)
            ).fetchone(),
            (old_blob_id, generation),
        )

        self.assertEqual(
            self.db.execute(
                "SELECT state,part_count FROM blobs WHERE blob_id=?",
                (completion_blob_id,),
            ).fetchone(),
            ("reserved", None),
        )

        self.assertEqual(
            self.db.execute(
                "SELECT part_number FROM blob_parts WHERE blob_id=?",
                (completion_blob_id,),
            ).fetchall(),
            [],
        )

        self.assert_healthy()

    def test_completed_part_snapshot_survives_replacement_and_reopen(self) -> None:
        completion_blob_id = self.insert_blob(
            write_kind="complete_multipart_upload", part_count=1
        )

        self.insert(
            "blob_parts",
            blob_id=completion_blob_id,
            part_number=7,
            part_index=1,
            byte_offset=0,
            byte_length=3,
            checksums_json="{}",
        )

        self.insert_object(blob_id=completion_blob_id)
        replacement_blob_id = self.insert_blob()

        with tempfile.TemporaryDirectory(prefix="mikrio-parts-") as directory:
            path = Path(directory) / "metadata.sqlite3"

            with closing(connect(path)) as writer:
                self.db.backup(writer)
                writer.execute("PRAGMA journal_mode=WAL")

                with closing(connect(path)) as reader:
                    query = (
                        "SELECT objects.blob_id,blobs.part_count,blob_parts.part_number,blob_parts.byte_length FROM objects "
                        "LEFT JOIN blobs ON blobs.blob_id=objects.blob_id "
                        "LEFT JOIN blob_parts ON blob_parts.blob_id=blobs.blob_id WHERE objects.key=?"
                    )

                    reader.execute("BEGIN")
                    expected = [(completion_blob_id, 1, 7, 3)]

                    self.assertEqual(
                        reader.execute(query, (b"key",)).fetchall(), expected
                    )

                    writer.execute("BEGIN IMMEDIATE")

                    writer.execute(
                        "UPDATE objects SET blob_id=?,generation=? WHERE key=?",
                        (replacement_blob_id, new_identity(), b"key"),
                    )

                    writer.execute(
                        "UPDATE blobs SET state='deleting',gc_pending=1 WHERE blob_id=?",
                        (completion_blob_id,),
                    )

                    writer.execute(
                        "DELETE FROM blob_parts WHERE blob_id=?", (completion_blob_id,)
                    )

                    writer.execute(
                        "DELETE FROM blobs WHERE blob_id=?", (completion_blob_id,)
                    )

                    writer.execute("COMMIT")

                    self.assertEqual(
                        reader.execute(query, (b"key",)).fetchall(), expected
                    )

                    reader.execute("COMMIT")

                    self.assertEqual(
                        reader.execute(query, (b"key",)).fetchall(),
                        [(replacement_blob_id, None, None, None)],
                    )

            with closing(connect(path)) as reopened:
                self.assertEqual(
                    reopened.execute(query, (b"key",)).fetchall(),
                    [(replacement_blob_id, None, None, None)],
                )

                self.assertEqual(
                    reopened.execute("PRAGMA foreign_key_check").fetchall(), []
                )

    def test_completed_part_pages_and_bounded_retirement(self) -> None:
        completion_blob_id = self.insert_blob(
            write_kind="complete_multipart_upload", part_count=10000, byte_length=30000
        )

        self.db.execute("BEGIN IMMEDIATE")

        self.db.executemany(
            "INSERT INTO blob_parts (blob_id,part_number,byte_offset,byte_length,checksums_json,part_index) VALUES (?,?,?,?,?,?)",
            (
                (
                    completion_blob_id,
                    part_number,
                    (part_number - 1) * 3,
                    3,
                    "{}",
                    part_number,
                )
                for part_number in range(1, 10001)
            ),
        )

        self.db.execute("COMMIT")

        page = self.db.execute(
            "SELECT part_number,byte_offset,byte_length FROM blob_parts "
            "WHERE blob_id=? AND part_number>? ORDER BY part_number LIMIT 1001",
            (completion_blob_id, 9000),
        ).fetchall()

        self.assertEqual(len(page), 1000)
        self.assertEqual((page[0], page[-1]), ((9001, 27000, 3), (10000, 29997, 3)))

        self.db.execute(
            "UPDATE blobs SET state='deleting',gc_pending=1 WHERE blob_id=?",
            (completion_blob_id,),
        )

        self.db.execute(
            "DELETE FROM blob_parts WHERE blob_id=? AND part_number IN "
            "(SELECT part_number FROM blob_parts WHERE blob_id=? ORDER BY part_number LIMIT 128)",
            (completion_blob_id, completion_blob_id),
        )

        self.assertEqual(
            self.db.execute(
                "SELECT count(*) FROM blob_parts WHERE blob_id=?", (completion_blob_id,)
            ).fetchone()[0],
            9872,
        )

        with self.rejected_input():
            self.db.execute("DELETE FROM blobs WHERE blob_id=?", (completion_blob_id,))

        # Remaining rows and deleting parent are durable work; a retry needs no new cursor/table.
        self.assertEqual(
            self.db.execute(
                "SELECT state,gc_pending FROM blobs WHERE blob_id=?",
                (completion_blob_id,),
            ).fetchone(),
            ("deleting", 1),
        )

        self.assert_healthy()

        delete_batch_query = (
            "DELETE FROM blob_parts WHERE blob_id=? AND part_number IN "
            "(SELECT part_number FROM blob_parts WHERE blob_id=? ORDER BY part_number LIMIT 128)"
        )

        with tempfile.TemporaryDirectory(prefix="mikrio-part-retirement-") as directory:
            path = Path(directory) / "metadata.sqlite3"

            with closing(connect(path)) as interrupted:
                self.db.backup(interrupted)
                interrupted.execute("PRAGMA journal_mode=WAL")
                interrupted.execute("BEGIN IMMEDIATE")

                removed_part_count = interrupted.execute(
                    delete_batch_query, (completion_blob_id, completion_blob_id)
                ).rowcount

                self.assertEqual(removed_part_count, 128)
                # Closing without COMMIT models a rolled-back metadata batch, not a process crash.

            with closing(connect(path)) as resumed:
                remaining_part_count = resumed.execute(
                    "SELECT count(*) FROM blob_parts WHERE blob_id=?",
                    (completion_blob_id,),
                ).fetchone()[0]

                self.assertEqual(remaining_part_count, 9872)

                self.assertEqual(
                    resumed.execute(
                        "SELECT state,gc_pending,part_count FROM blobs WHERE blob_id=?",
                        (completion_blob_id,),
                    ).fetchone(),
                    ("deleting", 1, 10000),
                )

                with self.assertRaises(sqlite3.IntegrityError):
                    resumed.execute(
                        "DELETE FROM blobs WHERE blob_id=?", (completion_blob_id,)
                    )

                # Finish the existing job from remaining rows, without restoring an in-memory cursor.
                for _ in range((remaining_part_count + 127) // 128):
                    resumed.execute("BEGIN IMMEDIATE")

                    removed_part_count = resumed.execute(
                        delete_batch_query, (completion_blob_id, completion_blob_id)
                    ).rowcount

                    self.assertEqual(removed_part_count, min(128, remaining_part_count))
                    remaining_part_count -= removed_part_count

                    if remaining_part_count == 0:
                        resumed.execute(
                            "DELETE FROM blobs WHERE blob_id=?", (completion_blob_id,)
                        )

                    resumed.execute("COMMIT")

                self.assertEqual(remaining_part_count, 0)

                self.assertEqual(
                    resumed.execute(
                        "SELECT blob_id FROM blobs WHERE blob_id=?",
                        (completion_blob_id,),
                    ).fetchall(),
                    [],
                )

                self.assertEqual(
                    resumed.execute("PRAGMA foreign_key_check").fetchall(), []
                )

    def test_query_plans_use_required_indexes(self) -> None:
        blob_id, _ = self.insert_object()
        principal_id = self.insert_principal()
        access_key_id = self.insert_access_key(principal_id)

        queries = [
            (
                "SELECT bucket_id FROM buckets WHERE name>? ORDER BY name LIMIT 10",
                ("a",),
                "buckets_name_uq",
            ),
            (
                "SELECT bucket_id FROM buckets WHERE name IS NULL AND bucket_id>? ORDER BY bucket_id LIMIT 10",
                (b"",),
                "buckets_retiring_idx",
            ),
            (
                "SELECT rule_index FROM bucket_lifecycle_rules WHERE bucket_id=? AND rule_id=?",
                (self.bucket_id, "expire"),
                "bucket_lifecycle_rules_rule_id_uq",
            ),
            (
                "SELECT principal_id FROM access_keys WHERE access_key_id=?",
                (access_key_id,),
                "PRIMARY KEY",
            ),
            (
                "SELECT part_number,byte_offset,byte_length FROM blob_parts WHERE blob_id=? AND part_index BETWEEN ? AND ? ORDER BY part_index",
                (blob_id, 8999, 9001),
                "blob_parts_order_uq",
            ),
            (
                "SELECT access_key_id FROM access_keys WHERE principal_id=? ORDER BY access_key_id",
                (principal_id,),
                "access_keys_principal_idx",
            ),
            (
                "SELECT key FROM objects WHERE bucket_id=? AND key>=? AND key<? ORDER BY key LIMIT 10",
                (self.bucket_id, b"a", b"z"),
                "sqlite_autoindex_objects_1",
            ),
            (
                "SELECT 1 FROM objects WHERE blob_id=? LIMIT 1",
                (blob_id,),
                "objects_blob_id_uq",
            ),
            (
                "SELECT part_number,byte_offset,byte_length FROM blob_parts WHERE blob_id=? AND part_number>? ORDER BY part_number LIMIT 1001",
                (blob_id, 0),
                "PRIMARY KEY",
            ),
            (
                "SELECT 1 FROM multipart_parts WHERE blob_id=? LIMIT 1",
                (blob_id,),
                "multipart_parts_blob_id_uq",
            ),
            (
                "SELECT row_id FROM blobs WHERE gc_pending=1 AND shard=? AND row_id>? AND row_id<=? ORDER BY row_id LIMIT 10",
                (0, 0, 100),
                "blobs_gc_idx",
            ),
            (
                "SELECT upload_id FROM multipart_uploads WHERE bucket_id=? AND key>=? ORDER BY key,upload_id LIMIT 10",
                (self.bucket_id, b"a"),
                "multipart_uploads_listing_idx",
            ),
            (
                "SELECT upload_id FROM multipart_uploads WHERE completion_blob_id=?",
                (blob_id,),
                "multipart_uploads_completion_blob_id_uq",
            ),
            (
                "SELECT row_id,key FROM objects WHERE row_id>? AND row_id<=? ORDER BY row_id LIMIT 10",
                (0, 100),
                "INTEGER PRIMARY KEY",
            ),
        ]

        for query, values, expected_index in queries:
            with self.subTest(query=query):
                plan = " ".join(
                    row[3]
                    for row in self.db.execute("EXPLAIN QUERY PLAN " + query, values)
                )

                self.assertIn("SEARCH ", plan)
                self.assertIn(expected_index, plan)
                self.assertNotIn("TEMP B-TREE", plan)

    def test_lifecycle_presence_queries_use_partial_indexes(self) -> None:
        for field, index in (
            ("expiration_days", "bucket_lifecycle_rules_expiration_idx"),
            ("abort_incomplete_multipart_days", "bucket_lifecycle_rules_abort_idx"),
        ):
            with self.subTest(field=field):
                query = (
                    "SELECT 1 FROM bucket_lifecycle_rules AS r JOIN buckets AS b "
                    "ON b.bucket_id=r.bucket_id WHERE r.is_enabled=1 "
                    f"AND r.{field} IS NOT NULL AND b.name IS NOT NULL "
                    "AND b.lifecycle_generation IS NOT NULL LIMIT 1"
                )

                plan = " ".join(
                    row[3] for row in self.db.execute("EXPLAIN QUERY PLAN " + query)
                )

                self.assertIn(f"SCAN r USING INDEX {index}", plan)
                self.assertIn("SEARCH b USING INDEX", plan)
                self.assertNotIn("AUTOMATIC", plan)
                self.assertNotIn("TEMP B-TREE", plan)

    def test_foreign_key_child_lookups_have_declared_indexes(self) -> None:
        for table in TABLES:
            columns = {
                row[1]: row[2] for row in self.db.execute(f"PRAGMA table_info({table})")
            }

            keys: dict[int, list[str]] = {}

            for key in self.db.execute(f"PRAGMA foreign_key_list({table})"):
                keys.setdefault(key[0], []).append(key[3])

            for fields in keys.values():
                with self.subTest(table=table, fields=fields):
                    predicate = " AND ".join(f"{field}=?" for field in fields)

                    values = tuple(
                        new_upload_id() if columns[field] == "TEXT" else new_identity()
                        for field in fields
                    )

                    plan = " ".join(
                        row[3]
                        for row in self.db.execute(
                            f"EXPLAIN QUERY PLAN SELECT 1 FROM {table} WHERE {predicate}",
                            values,
                        )
                    )

                    self.assertIn("SEARCH ", plan)
                    self.assertNotIn("SCAN ", plan)
                    self.assertNotIn("AUTOMATIC", plan)
                    self.assertNotIn("TEMP B-TREE", plan)

    def test_wal_commit_witness_and_migration_visibility(self) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-schema-") as directory:
            path = Path(directory) / "metadata.sqlite3"

            with closing(connect(path)) as writer:
                self.db.backup(writer)

                self.assertEqual(
                    writer.execute("PRAGMA journal_mode=WAL").fetchone()[0], "wal"
                )

                writer.execute(
                    "INSERT INTO store_metadata(singleton_id,store_id,owner_account_id,canonical_owner_id,format_version,region,"
                    "shard_count,shard_algorithm,continuation_token_key,created_at_ms) "
                    "VALUES(1,?,?,?,1,'us-east-1',256,'first_byte',?,0)",
                    (
                        new_identity(),
                        self.owner_account_id,
                        self.canonical_owner_id,
                        os.urandom(32),
                    ),
                )

                with closing(connect(path)) as reader:
                    writer.execute("BEGIN IMMEDIATE")

                    writer.execute(
                        "UPDATE buckets SET cors_rules_json='[]' WHERE bucket_id=?",
                        (self.bucket_id,),
                    )

                    self.assertEqual(
                        writer.execute(
                            "SELECT cors_rules_json FROM buckets"
                        ).fetchone()[0],
                        "[]",
                    )

                    reader.execute("BEGIN")

                    self.assertIsNone(
                        reader.execute(
                            "SELECT cors_rules_json FROM buckets"
                        ).fetchone()[0]
                    )

                    writer.execute("COMMIT")

                    self.assertIsNone(
                        reader.execute(
                            "SELECT cors_rules_json FROM buckets"
                        ).fetchone()[0],
                        "old snapshot is not outcome evidence",
                    )

                    reader.execute("COMMIT")

                    self.assertEqual(
                        reader.execute(
                            "SELECT cors_rules_json FROM buckets"
                        ).fetchone()[0],
                        "[]",
                    )

                    # Synthetic DDL/version transition, not a registered migration.
                    for should_commit in (False, True):
                        with self.subTest(should_commit=should_commit):
                            writer.execute("BEGIN IMMEDIATE")

                            writer.execute(
                                "ALTER TABLE buckets ADD COLUMN future_value TEXT"
                            )

                            writer.execute(
                                "UPDATE store_metadata SET format_version=2 WHERE singleton_id=1"
                            )

                            self.assertNotIn(
                                "future_value",
                                [
                                    row[1]
                                    for row in reader.execute(
                                        "PRAGMA table_info(buckets)"
                                    )
                                ],
                            )

                            self.assertEqual(
                                reader.execute(
                                    "SELECT format_version FROM store_metadata"
                                ).fetchone()[0],
                                1,
                            )

                            writer.execute("COMMIT" if should_commit else "ROLLBACK")

                            self.assertEqual(
                                "future_value"
                                in [
                                    row[1]
                                    for row in reader.execute(
                                        "PRAGMA table_info(buckets)"
                                    )
                                ],
                                should_commit,
                            )

                            self.assertEqual(
                                reader.execute(
                                    "SELECT format_version FROM store_metadata"
                                ).fetchone()[0],
                                2 if should_commit else 1,
                            )


if __name__ == "__main__":
    if sqlite3.sqlite_version_info < (3, 37, 0):
        raise SystemExit("Schema verification needs SQLite 3.37+ for STRICT tables.")

    print(f"Current-schema checks using SQLite {sqlite3.sqlite_version}", flush=True)
    unittest.main(verbosity=2)
