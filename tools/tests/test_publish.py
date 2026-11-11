"""Registry publication preserves reviewed bytes and resumes missing uploads."""

from __future__ import annotations

import base64
import hashlib
import io
import json
import struct
import tarfile
import tempfile
import unittest
import urllib.parse
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
    def test_npm_resume_publishes_missing_native_packages_before_the_wrapper(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            records = []

            for name in (
                "mikrio",
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

            self.assertEqual(uploaded, ["@mikrio/node-linux-arm64-gnu", "mikrio"])

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
