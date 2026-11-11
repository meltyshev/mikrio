"""CORS ownership and whole-value replacement through an installed wheel."""

import tempfile
import unittest
from collections.abc import Callable
from pathlib import Path

from mikrio import CorsRule, StorageError, Store


class CorsTests(unittest.TestCase):
    def assert_cors_failures(
        self,
        store: Store,
        bucket: str,
        rules: list[CorsRule],
        expected_owner: str | None,
        code: str,
    ) -> None:
        operations: list[tuple[str, Callable[[], object], str | None]] = [
            (
                "put",
                lambda: store.put_bucket_cors(
                    bucket=bucket,
                    cors_rules=rules,
                    expected_bucket_owner=expected_owner,
                ),
                "NotCommitted",
            ),
            (
                "get",
                lambda: store.get_bucket_cors(
                    bucket=bucket, expected_bucket_owner=expected_owner
                ),
                None,
            ),
            (
                "delete",
                lambda: store.delete_bucket_cors(
                    bucket=bucket, expected_bucket_owner=expected_owner
                ),
                "NotCommitted",
            ),
        ]

        for name, operation, outcome in operations:
            with self.subTest(operation=name, code=code, owner=expected_owner):
                with self.assertRaises(StorageError) as failed:
                    operation()

                self.assertEqual(failed.exception.code, code)
                self.assertEqual(failed.exception.mutation_outcome, outcome)

    def test_rules_are_detached_and_preserve_optional_empty_values(self) -> None:
        origins = ["https://app.example"]

        first = CorsRule(
            id="browser-😀",
            allowed_origins=origins,
            allowed_methods=["GET", "PUT"],
            allowed_headers=[],
            expose_headers=["ETag"],
            max_age_seconds=0,
        )

        second = CorsRule(allowed_origins=["*"], allowed_methods=["HEAD"])
        origins[0] = "https://changed.example"

        with tempfile.TemporaryDirectory(prefix="mikrio-cors-") as temporary:
            path = Path(temporary) / "store"

            with (
                Store.open(directory=path) as store,
                Store.open(directory=path) as peer,
            ):
                store.create_bucket(bucket="photos")

                with self.assertRaises(StorageError) as missing:
                    store.get_bucket_cors(bucket="photos")

                self.assertEqual(missing.exception.code, "NoSuchCORSConfiguration")
                owner = store.get_owner_info().account_id

                store.put_bucket_cors(
                    bucket="photos",
                    cors_rules=[first, second],
                    expected_bucket_owner=owner,
                )

                result = peer.get_bucket_cors(
                    bucket="photos", expected_bucket_owner=owner
                )

                self.assertIsInstance(result.cors_rules, tuple)
                stored_rule = result.cors_rules[0]
                self.assertEqual(stored_rule.id, "browser-😀")
                self.assertEqual(stored_rule.allowed_origins, ("https://app.example",))
                self.assertEqual(stored_rule.allowed_methods, ("GET", "PUT"))
                self.assertEqual(stored_rule.allowed_headers, ())
                self.assertEqual(stored_rule.expose_headers, ("ETag",))
                self.assertEqual(stored_rule.max_age_seconds, 0)
                self.assertIsNone(result.cors_rules[1].allowed_headers)
                self.assertIsNone(result.cors_rules[1].max_age_seconds)

                with self.assertRaises(AttributeError):
                    stored_rule.id = "changed"  # type: ignore[misc]

                with self.assertRaises(StorageError) as invalid:
                    store.put_bucket_cors(bucket="photos", cors_rules=[first, first])

                self.assertEqual(invalid.exception.code, "InvalidArgument")
                self.assertEqual(invalid.exception.mutation_outcome, "NotCommitted")

                wrong_owner = (
                    "000000000000" if owner != "000000000000" else "111111111111"
                )

                self.assert_cors_failures(
                    store, "photos", [first], wrong_owner, "AccessDenied"
                )

                self.assert_cors_failures(
                    store, "photos", [first], "", "InvalidArgument"
                )

                self.assert_cors_failures(
                    store, "missing", [first], owner, "NoSuchBucket"
                )

                self.assertEqual(
                    peer.get_bucket_cors(bucket="photos").cors_rules[0].id,
                    stored_rule.id,
                )

            self.assertEqual(stored_rule.allowed_origins, ("https://app.example",))

            with Store.open(directory=path) as reopened:
                self.assertEqual(
                    reopened.get_bucket_cors(bucket="photos").cors_rules[0].id,
                    stored_rule.id,
                )

                for _ in range(2):
                    reopened.delete_bucket_cors(
                        bucket="photos", expected_bucket_owner=owner
                    )

                with self.assertRaises(StorageError) as missing:
                    reopened.get_bucket_cors(bucket="photos")

                self.assertEqual(missing.exception.code, "NoSuchCORSConfiguration")

            self.assert_cors_failures(reopened, "photos", [first], None, "StoreClosed")

    def test_strict_host_types_and_native_rule_validation(self) -> None:
        for seconds in [-(2**31), -1, 0, 2**31 - 1]:
            with self.subTest(max_age_seconds=seconds):
                self.assertEqual(
                    CorsRule(
                        allowed_origins=["*"],
                        allowed_methods=["GET"],
                        max_age_seconds=seconds,
                    ).max_age_seconds,
                    seconds,
                )

        with self.assertRaises(TypeError):
            CorsRule(
                allowed_origins=["*"], allowed_methods=["GET"], max_age_seconds=True
            )

        for seconds in [-(2**31) - 1, 2**31, 2**64]:
            with self.subTest(seconds=seconds):
                with self.assertRaises(StorageError) as invalid:
                    CorsRule(
                        allowed_origins=["*"],
                        allowed_methods=["GET"],
                        max_age_seconds=seconds,
                    )

                self.assertEqual(invalid.exception.code, "InvalidArgument")

        with self.assertRaises(StorageError) as invalid:
            CorsRule(allowed_origins=["*"], allowed_methods=["OPTIONS"])

        self.assertEqual(invalid.exception.code, "InvalidArgument")

        with self.assertRaises(StorageError) as invalid:
            CorsRule(allowed_origins=["*"], allowed_methods=["GET"], id="\ud800")

        self.assertEqual(invalid.exception.code, "InvalidArgument")

    def test_rule_fields_and_exact_configuration_limits(self) -> None:
        for identifier in ["😀" * 256, "control\0"]:
            with self.subTest(id_length=len(identifier)):
                with self.assertRaises(StorageError) as invalid:
                    CorsRule(
                        allowed_origins=["*"], allowed_methods=["GET"], id=identifier
                    )

                self.assertEqual(invalid.exception.code, "InvalidArgument")

        for origins in [[], [""], ["a b"], ["é"], ["**"], ["x\r\n"]]:
            with self.subTest(origins=origins):
                with self.assertRaises(StorageError) as invalid:
                    CorsRule(allowed_origins=origins, allowed_methods=["GET"])

                self.assertEqual(invalid.exception.code, "InvalidArgument")

        for methods in [[], ["get"], ["OPTIONS"], ["PATCH"]]:
            with self.subTest(methods=methods):
                with self.assertRaises(StorageError) as invalid:
                    CorsRule(allowed_origins=["*"], allowed_methods=methods)

                self.assertEqual(invalid.exception.code, "InvalidArgument")

        for header in ["", "x:y", "**", "é", "x\r\n"]:
            with self.subTest(allowed_header=header):
                with self.assertRaises(StorageError) as invalid:
                    CorsRule(
                        allowed_origins=["*"],
                        allowed_methods=["GET"],
                        allowed_headers=[header],
                    )

                self.assertEqual(invalid.exception.code, "InvalidArgument")

        for header in ["*", "x:y", "é"]:
            with self.subTest(expose_header=header):
                with self.assertRaises(StorageError) as invalid:
                    CorsRule(
                        allowed_origins=["*"],
                        allowed_methods=["GET"],
                        expose_headers=[header],
                    )

                self.assertEqual(invalid.exception.code, "InvalidArgument")

        temporary = tempfile.TemporaryDirectory(prefix="mikrio-cors-limits-")
        self.addCleanup(temporary.cleanup)
        directory = Path(temporary.name)

        with Store.open(directory=directory / "store") as store:
            store.create_bucket(bucket="photos")

            rule = CorsRule(
                id="😀" * 255,
                allowed_origins=["https://*.example.com"],
                allowed_methods=["GET", "PUT", "HEAD", "POST", "DELETE"],
                allowed_headers=["x-amz-*", "Content-Type"],
                expose_headers=["ETag", "Content-Disposition"],
                max_age_seconds=-(2**31),
            )

            store.put_bucket_cors(bucket="photos", cors_rules=[rule])

            self.assertEqual(
                store.get_bucket_cors(bucket="photos").cors_rules[0].id, rule.id
            )

            anonymous = CorsRule(allowed_origins=["*"], allowed_methods=["GET"])
            store.put_bucket_cors(bucket="photos", cors_rules=[anonymous] * 100)

            self.assertEqual(
                len(store.get_bucket_cors(bucket="photos").cors_rules), 100
            )

            with self.assertRaises(StorageError) as invalid:
                store.put_bucket_cors(bucket="photos", cors_rules=[anonymous] * 101)

            self.assertEqual(invalid.exception.code, "InvalidArgument")
            self.assertEqual(invalid.exception.mutation_outcome, "NotCommitted")

            self.assertEqual(
                len(store.get_bucket_cors(bucket="photos").cors_rules), 100
            )

            empty_configuration = (
                '<CORSConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
                "<CORSRule>"
                "<AllowedOrigin>"
                "</AllowedOrigin>"
                "<AllowedMethod>GET</AllowedMethod>"
                "</CORSRule>"
                "</CORSConfiguration>"
            )

            max_origin_bytes = 65_536 - len(empty_configuration.encode())

            for origin_bytes in [max_origin_bytes - 1, max_origin_bytes]:
                with self.subTest(origin_bytes=origin_bytes):
                    boundary = CorsRule(
                        allowed_origins=["\\" * origin_bytes], allowed_methods=["GET"]
                    )

                    store.put_bucket_cors(bucket="photos", cors_rules=[boundary])

                    self.assertEqual(
                        store.get_bucket_cors(bucket="photos")
                        .cors_rules[0]
                        .allowed_origins,
                        boundary.allowed_origins,
                    )

            with self.assertRaises(StorageError) as invalid:
                CorsRule(
                    allowed_origins=["\\" * (max_origin_bytes + 1)],
                    allowed_methods=["GET"],
                )

            self.assertEqual(invalid.exception.code, "InvalidArgument")

            self.assertEqual(
                len(
                    store.get_bucket_cors(bucket="photos")
                    .cors_rules[0]
                    .allowed_origins[0]
                ),
                max_origin_bytes,
            )
