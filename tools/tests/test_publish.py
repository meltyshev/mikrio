"""Registry publication preserves reviewed bytes and resumes missing uploads."""

from __future__ import annotations

import base64
import hashlib
import io
import json
import struct
import subprocess
import tarfile
import tempfile
import unittest
import urllib.error
import urllib.parse
from email.message import Message
from pathlib import Path
from unittest.mock import patch

from tools import publish
from tools.artifacts import digest


def write_archive(path: Path, members: list[tuple[str, bytes]]) -> None:
    with tarfile.open(path, "w:gz") as stream:
        for name, payload in members:
            info = tarfile.TarInfo(name)
            info.size = len(payload)
            stream.addfile(info, io.BytesIO(payload))


class PublishTests(unittest.TestCase):
    def test_registry_errors_preserve_bounded_details_without_retrying_uploads(
        self,
    ) -> None:
        url = "https://crates.io/api/v1/crates/new"

        for code, body in (
            (429, b"upload"),
            (403, b"upload"),
            (404, b"upload"),
            (404, None),
        ):
            with self.subTest(code=code, body=body):
                headers = Message()
                headers["Retry-After"] = "600"
                response = io.BytesIO(b"try later secret-token\n\x1b" + b"x" * 5000)
                error = urllib.error.HTTPError(url, code, "failure", headers, response)

                with patch("tools.publish.urllib.request.build_opener") as opener:
                    opener.return_value.open.side_effect = error

                    if code == 404 and body is None:
                        self.assertIsNone(publish.registry_request(url))
                    else:
                        with self.assertRaises(RuntimeError) as caught:
                            publish.registry_request(
                                url, body=body, token="secret-token"
                            )

                        message = str(caught.exception)
                        self.assertIn(f"HTTP {code}", message)
                        self.assertIn("Retry-After: '600'", message)
                        self.assertIn("try later [redacted]", message)
                        self.assertNotIn("secret-token", message)
                        self.assertNotIn("\n", message)
                        self.assertNotIn("\x1b", message)
                        self.assertLess(len(message), 4300)

                    opener.return_value.open.assert_called_once()

    def test_npm_processing_waits_for_all_natives_before_uploading_the_wrapper(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)

            records = [
                {"kind": "npm", "name": name, "version": "0.1.0", "file": filename}
                for name, filename in (
                    ("@mikrio/node", "mikrio-node-0.1.0.tgz"),
                    ("@mikrio/node-darwin-arm64", "mikrio-node-darwin-arm64-0.1.0.tgz"),
                    (
                        "@mikrio/node-linux-arm64-gnu",
                        "mikrio-node-linux-arm64-gnu-0.1.0.tgz",
                    ),
                )
            ]

            now = 0.0
            accepted: dict[str, float] = {}

            processing_seconds = {
                "@mikrio/node": 600,
                "@mikrio/node-darwin-arm64": 300,
                "@mikrio/node-linux-arm64-gnu": 900,
            }

            for package in records:
                (directory / package["file"]).write_bytes(package["name"].encode())

            def registry_response(url: str) -> bytes | None:
                name = urllib.parse.unquote(url.split("/")[-2])

                if name not in accepted:
                    return None

                if now - accepted[name] < processing_seconds[name]:
                    return None

                checksum = base64.b64encode(
                    hashlib.sha512(name.encode()).digest()
                ).decode()

                return json.dumps(
                    {"dist": {"integrity": "sha512-" + checksum}}
                ).encode()

            def upload(command: list[str], *, check: bool) -> None:
                name = Path(command[2]).read_text(encoding="utf-8")
                self.assertNotIn(name, accepted)

                if name == "@mikrio/node":
                    self.assertEqual(len(accepted), 2)
                    self.assertGreaterEqual(now, 900)

                accepted[name] = now

            def sleep(seconds: float) -> None:
                nonlocal now
                now += seconds

            with (
                patch.object(
                    publish, "registry_request", side_effect=registry_response
                ),
                patch("tools.publish.subprocess.run", side_effect=upload) as command,
                patch("tools.publish.time.monotonic", side_effect=lambda: now),
                patch("tools.publish.time.sleep", side_effect=sleep),
            ):
                publish.publish_npm(directory, records)
                publish.publish_npm(directory, records)
                self.assertEqual(command.call_count, 3)

            self.assertEqual(now, 1500)
            self.assertEqual(list(accepted)[-1], "@mikrio/node")

    def test_npm_processing_rejects_different_bytes_before_uploading_the_wrapper(
        self,
    ) -> None:
        records = [
            {
                "kind": "npm",
                "name": "@mikrio/node",
                "version": "0.1.0",
                "file": "mikrio-node-0.1.0.tgz",
            },
            {
                "kind": "npm",
                "name": "@mikrio/node-darwin-arm64",
                "version": "0.1.0",
                "file": "mikrio-node-darwin-arm64-0.1.0.tgz",
            },
        ]

        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            native = directory / "mikrio-node-darwin-arm64-0.1.0.tgz"
            native.write_bytes(b"reviewed native archive")

            with (
                patch.object(
                    publish,
                    "registry_request",
                    side_effect=(None, b'{"dist":{"integrity":"wrong"}}'),
                ),
                patch("tools.publish.subprocess.run") as command,
                patch("tools.publish.time.sleep") as sleep,
                self.assertRaisesRegex(ValueError, "different bytes"),
            ):
                publish.publish_npm(directory, records)

            command.assert_called_once()
            self.assertEqual(Path(command.call_args.args[0][2]), native.resolve())
            sleep.assert_not_called()

    def test_npm_processing_timeout_preserves_pending_uploads_and_blocks_the_wrapper(
        self,
    ) -> None:
        records = [
            {"kind": "npm", "name": "@mikrio/node", "file": "mikrio-node-0.1.0.tgz"},
            {"kind": "npm", "name": "@mikrio/node-darwin-arm64", "file": "native.tgz"},
        ]

        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(publish, "is_npm_published", return_value=False),
            patch("tools.publish.subprocess.run") as command,
            patch("tools.publish.time.monotonic", side_effect=(0, 1195, 1200)),
            patch("tools.publish.time.sleep") as sleep,
            self.assertRaisesRegex(TimeoutError, "native.tgz.*same release archives"),
        ):
            publish.publish_npm(Path(temporary), records)

        command.assert_called_once()
        self.assertEqual(Path(command.call_args.args[0][2]).name, "native.tgz")
        sleep.assert_called_once_with(5)

    def test_npm_rejection_is_not_retried_or_treated_as_pending(self) -> None:
        records = [
            {"kind": "npm", "name": "@mikrio/node", "file": "mikrio-node-0.1.0.tgz"}
        ]

        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(publish, "is_npm_published", return_value=False),
            patch(
                "tools.publish.subprocess.run",
                side_effect=subprocess.CalledProcessError(1, "npm publish"),
            ) as command,
            patch("tools.publish.time.sleep") as sleep,
            self.assertRaises(subprocess.CalledProcessError),
        ):
            publish.publish_npm(Path(temporary), records)

        command.assert_called_once()
        sleep.assert_not_called()

    def test_npm_resume_publishes_missing_native_packages_before_the_wrapper(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            records = []

            for name in (
                "@mikrio/node",
                "@mikrio/node-linux-arm64-gnu",
                "@mikrio/node-darwin-arm64",
            ):
                filename = name.replace("@", "").replace("/", "-") + "-0.1.0.tgz"
                (directory / filename).write_bytes(name.encode())

                records.append(
                    {"kind": "npm", "name": name, "version": "0.1.0", "file": filename}
                )

            existing = {"@mikrio/node-darwin-arm64"}
            uploaded = []

            def registry_response(url: str) -> bytes | None:
                name = urllib.parse.unquote(url.split("/")[-2])

                if name not in existing:
                    return None

                checksum = base64.b64encode(
                    hashlib.sha512(name.encode()).digest()
                ).decode()

                return json.dumps(
                    {"dist": {"integrity": "sha512-" + checksum}}
                ).encode()

            def upload(command: list[str], *, check: bool) -> None:
                path = Path(command[2])
                name = path.read_text(encoding="utf-8")
                self.assertIn("--ignore-scripts", command)
                self.assertTrue(check)
                uploaded.append(name)
                existing.add(name)

            with (
                patch.object(
                    publish, "registry_request", side_effect=registry_response
                ),
                patch("tools.publish.subprocess.run", side_effect=upload),
            ):
                publish.publish_npm(directory, records)
                publish.publish_npm(directory, records)

            self.assertEqual(uploaded, ["@mikrio/node-linux-arm64-gnu", "@mikrio/node"])

            with (
                patch.object(
                    publish,
                    "registry_request",
                    return_value=b'{"dist":{"integrity":"wrong"}}',
                ),
                patch("tools.publish.subprocess.run") as command,
                self.assertRaisesRegex(ValueError, "different bytes"),
            ):
                publish.publish_npm(directory, records)

            command.assert_not_called()

    def test_existing_crate_reuse_requires_identical_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            local = root / "mikrio-0.1.0.crate"
            remote = root / "remote.crate"
            prefix = "mikrio-0.1.0/"

            write_archive(
                local,
                [
                    (prefix + "lib.rs", b"source"),
                    (prefix + ".cargo_vcs_info.json", b"new revision"),
                ],
            )

            write_archive(
                remote,
                [
                    (prefix + "lib.rs", b"source"),
                    (prefix + ".cargo_vcs_info.json", b"original revision"),
                ],
            )

            item = {"name": "mikrio", "version": "0.1.0", "file": local.name}

            with (
                patch.object(publish, "rust_publish_order", return_value=[item]),
                patch.object(
                    publish,
                    "find_published_crate",
                    return_value={"cksum": digest(remote)},
                ),
                patch.object(
                    publish, "registry_request", return_value=remote.read_bytes()
                ),
            ):
                publish.reuse_crates(root)
                self.assertEqual(local.read_bytes(), remote.read_bytes())
                write_archive(local, [(prefix + "lib.rs", b"changed source")])

                with self.assertRaisesRegex(ValueError, "without a version bump"):
                    publish.reuse_crates(root)

    def test_crate_metadata_keeps_renamed_target_build_dependencies(self) -> None:
        manifest = b"""[package]
name = "mikrio-example"
version = "0.1.0"
readme = "README.md"
license = "Apache-2.0"
[target.'cfg(unix)'.build-dependencies.alias]
package = "upstream"
version = "=1.2.3"
default-features = false
features = ["selected"]
[features]
optional = ["dep:alias"]
"""

        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "mikrio-example-0.1.0.crate"

            write_archive(
                path,
                [
                    ("mikrio-example-0.1.0/Cargo.toml", manifest),
                    ("mikrio-example-0.1.0/README.md", b"Description"),
                ],
            )

            metadata = publish.crate_upload_metadata(path)
            self.assertEqual(metadata["readme"], "Description")

            self.assertEqual(
                metadata["deps"],
                [
                    {
                        "name": "upstream",
                        "version_req": "=1.2.3",
                        "features": ["selected"],
                        "optional": False,
                        "default_features": False,
                        "target": "cfg(unix)",
                        "kind": "build",
                        "registry": None,
                        "explicit_name_in_toml": "alias",
                    }
                ],
            )

    def test_existing_crate_mismatch_prevents_any_upload(self) -> None:
        item = {
            "kind": "crates",
            "name": "mikrio",
            "version": "0.1.0",
            "file": "mikrio-0.1.0.crate",
            "sha256": "expected",
        }

        with (
            patch.object(publish, "rust_publish_order", return_value=[item]),
            patch.object(
                publish, "find_published_crate", return_value={"cksum": "different"}
            ),
            patch.object(publish, "registry_request") as request,
        ):
            with self.assertRaisesRegex(ValueError, "different bytes"):
                publish.publish_crates(Path("unused"), [item])

            request.assert_not_called()

    def test_pypi_resume_stages_only_missing_matching_files(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first, second = root / "first.whl", root / "second.whl"
            first.write_bytes(b"first")
            second.write_bytes(b"second")

            records = [
                {
                    "kind": "pypi",
                    "version": "0.1.0",
                    "file": path.name,
                    "sha256": digest(path),
                }
                for path in (first, second)
            ]

            with (
                patch.object(
                    publish, "pypi_checksums", return_value={first.name: digest(first)}
                ),
                patch.object(publish, "write_output"),
            ):
                publish.prepare_pypi(root, records, root / "upload")

            self.assertEqual(
                [path.name for path in (root / "upload").iterdir()], [second.name]
            )

            with patch.object(
                publish, "pypi_checksums", return_value={first.name: "wrong"}
            ):
                with self.assertRaisesRegex(ValueError, "different bytes"):
                    publish.prepare_pypi(root, records, root / "refused")

    def test_crate_upload_preserves_the_reviewed_bytes_and_does_not_retry(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "mikrio-0.1.0.crate"
            path.write_bytes(b"exact reviewed archive")

            item = {
                "kind": "crates",
                "name": "mikrio",
                "version": "0.1.0",
                "file": path.name,
                "sha256": digest(path),
            }

            with (
                patch.dict("os.environ", {"CARGO_REGISTRY_TOKEN": "test-token"}),
                patch.object(publish, "rust_publish_order", return_value=[item]),
                patch.object(publish, "find_published_crate", return_value=None),
                patch.object(
                    publish, "crate_upload_metadata", return_value={"name": "mikrio"}
                ),
                patch.object(
                    publish,
                    "registry_request",
                    side_effect=TimeoutError("uncertain upload"),
                ) as request,
            ):
                with self.assertRaises(TimeoutError):
                    publish.publish_crates(root, [item])

                request.assert_called_once()
                body = request.call_args.kwargs["body"]
                length = struct.unpack_from("<I", body)[0]
                self.assertEqual(json.loads(body[4 : 4 + length]), {"name": "mikrio"})
                size = struct.unpack_from("<I", body, 4 + length)[0]
                self.assertEqual(body[8 + length :], path.read_bytes())
                self.assertEqual(size, path.stat().st_size)
