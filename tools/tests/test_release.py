"""Release assembly rejects incomplete or inconsistent distribution artifacts."""

from __future__ import annotations

import hashlib
import io
import json
import struct
import subprocess
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

from tools import release
from tools.artifacts import digest
from tools.native_runtime import TARGETS
from tools.node_packages import native_manifest, optional_dependencies
from tools.version import check


def write_archive(path: Path, members: list[tuple[str, bytes]]) -> None:
    with tarfile.open(path, "w:gz") as stream:
        for name, payload in members:
            info = tarfile.TarInfo(name)
            info.size = len(payload)
            stream.addfile(info, io.BytesIO(payload))


class ReleaseTests(unittest.TestCase):
    def test_repacked_linux_wheels_accept_directory_entries_during_assembly(
        self,
    ) -> None:
        for target, architecture, machine in (
            ("linux-arm64", "aarch64", 183),
            ("linux-x86_64", "x86_64", 62),
        ):
            with (
                self.subTest(target=target),
                tempfile.TemporaryDirectory() as temporary,
            ):
                path = (
                    Path(temporary)
                    / f"mikrio-0.1.0-cp312-abi3-manylinux_2_28_{architecture}.whl"
                )
                payload = bytearray(64)
                payload[:6] = b"\x7fELF\x02\x01"
                struct.pack_into("<H", payload, 18, machine)
                metadata_prefix = "mikrio-0.1.0.dist-info/"

                contents = {
                    "mikrio/": b"",
                    "mikrio/__init__.py": b"",
                    "mikrio/__init__.pyi": b"",
                    "mikrio/py.typed": b"",
                    "mikrio/THIRD_PARTY_LICENSES.txt": b"notices",
                    "mikrio/_native.abi3.so": bytes(payload),
                    "mikrio/libmikrio_sqlite3.so": bytes(payload),
                    metadata_prefix: b"",
                    metadata_prefix + "METADATA": b"Name: mikrio\nVersion: 0.1.0\n",
                    metadata_prefix + "WHEEL": b"audited tags",
                    metadata_prefix + "RECORD": b"audited records",
                    metadata_prefix + "licenses/": b"",
                    metadata_prefix + "licenses/LICENSE": b"license",
                }

                with zipfile.ZipFile(path, "w") as archive:
                    for name, content in contents.items():
                        archive.writestr(name, content)

                self.assertEqual(
                    release.inspect_wheel(path, "0.1.0"),
                    {"kind": "pypi", "name": "mikrio", "target": target},
                )

    def test_native_npm_package_requires_matching_receipt_and_libc(self) -> None:
        payload = bytearray(64)
        payload[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", payload, 18, 183)
        manifest = native_manifest("0.1.0", "linux-arm64", {"node": ">=22"})

        receipt = {
            "library": "libmikrio_sqlite3.so",
            "sha256": hashlib.sha256(payload).hexdigest(),
        }

        contents = {
            "package/package.json": json.dumps(manifest).encode(),
            "package/mikrio.node": bytes(payload),
            "package/libmikrio_sqlite3.so": bytes(payload),
            "package/mikrio-sqlite-runtime.json": json.dumps(receipt).encode(),
            "package/LICENSE": b"license",
            "package/README.md": b"Native package documentation",
            "package/THIRD_PARTY_LICENSES.txt": b"notices",
        }

        path = Path("mikrio-node-linux-arm64-gnu-0.1.0.tgz")
        release.inspect_npm(path, contents, "0.1.0")

        without_readme = dict(contents)
        del without_readme["package/README.md"]

        with self.assertRaisesRegex(ValueError, "missing its README"):
            release.inspect_npm(path, without_readme, "0.1.0")

        for field, changed in (("receipt", b"{}"), ("libc", ["musl"])):
            with self.subTest(field=field):
                altered = dict(contents)

                if field == "receipt":
                    altered["package/mikrio-sqlite-runtime.json"] = b"{}"
                else:
                    altered["package/package.json"] = json.dumps(
                        manifest | {"libc": changed}
                    ).encode()

                with self.assertRaisesRegex(ValueError, "receipt|platform metadata"):
                    release.inspect_npm(path, altered, "0.1.0")

    def test_release_notes_select_the_exact_version(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            changelog = root / "CHANGELOG.md"

            for heading in ("0.1.0", "[0.1.0] - 2026-11-11"):
                with self.subTest(heading=heading):
                    changelog.write_text(
                        "## 0.1.00\nWrong notes\n"
                        f"## {heading}\nReviewed [support](SUPPORT.md).\n"
                        "## 0.0.9\nOlder notes\n",
                        encoding="utf-8",
                        newline="\n",
                    )

                    with patch.object(release, "ROOT", root):
                        self.assertEqual(
                            release.release_notes("0.1.0"),
                            "Reviewed [support](https://github.com/"
                            "meltyshev/mikrio/blob/v0.1.0/SUPPORT.md).\n",
                        )

    def test_similar_versions_cannot_supply_missing_release_notes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            for heading in ("0.1.00", "[0.1.00]", "0.1.0-rc.1", "[0.1.0", "0.1.0]"):
                with self.subTest(heading=heading):
                    (root / "CHANGELOG.md").write_text(
                        f"## {heading}\nWrong notes\n", encoding="utf-8", newline="\n"
                    )

                    with (
                        patch.object(release, "ROOT", root),
                        self.assertRaisesRegex(ValueError, "missing release changelog"),
                    ):
                        release.release_notes("0.1.0")

    def test_changed_release_bytes_are_rejected_before_package_processing(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            path = directory / "archive.tgz"
            path.write_bytes(b"reviewed bytes")

            manifest = {
                "schema": 1,
                "version": "0.1.0",
                "revision": "reviewed",
                "packages": [{"file": path.name, "sha256": digest(path)}],
            }

            (directory / "release.json").write_text(
                json.dumps(manifest), encoding="utf-8", newline="\n"
            )

            (directory / "publish-order.json").write_text(
                "[]", encoding="utf-8", newline="\n"
            )

            (directory / "release-notes.md").write_text(
                "Reviewed notes", encoding="utf-8", newline="\n"
            )

            checksums = "".join(
                f"{digest(item)}  {item.name}\n" for item in directory.iterdir()
            )

            (directory / "SHA256SUMS").write_text(
                checksums, encoding="utf-8", newline="\n"
            )

            path.write_bytes(b"different bytes")

            with (
                patch.object(release, "check", return_value="0.1.0"),
                patch.object(release, "source_revision", return_value="reviewed"),
                patch.object(release, "inspect_package") as inspect_package,
            ):
                with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                    release.verify(directory)

                inspect_package.assert_not_called()

    def test_a_linked_checksum_file_is_not_an_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)

            (directory / "other").write_text(
                "unexpected input", encoding="utf-8", newline="\n"
            )

            (directory / "SHA256SUMS").symlink_to(directory / "other")

            with self.assertRaisesRegex(ValueError, "linked checksum"):
                release.verify(directory)

    def test_complete_inventory_rejects_a_missing_or_duplicate_target(self) -> None:
        records = [
            {"kind": "crates", "name": item["name"]}
            for item in release.rust_publish_order()
        ]

        records += [
            {"kind": "npm", "name": name}
            for name in ("mikrio", *optional_dependencies(check()))
        ]

        records += [
            {"kind": "pypi", "name": "mikrio", "target": target}
            for target in (*TARGETS, "source")
        ]

        records += [
            {"kind": kind, "name": "mikrio", "target": target}
            for kind in ("cli", "c")
            for target in TARGETS
        ]

        release.validate_inventory(records)

        with self.assertRaisesRegex(ValueError, "incomplete"):
            release.validate_inventory(records[:-1])

        with self.assertRaisesRegex(ValueError, "duplicate"):
            release.validate_inventory([*records, records[-1]])

    def test_archive_rejects_traversal_duplicates_and_links(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "test.tgz"

            for names in (
                ("package/../escape",),
                ("/absolute",),
                ("package/file", "package/file"),
            ):
                write_archive(path, [(name, b"payload") for name in names])

                with self.assertRaisesRegex(ValueError, "unsafe archive"):
                    release.read_tar_members(path)

            with tarfile.open(path, "w:gz") as stream:
                info = tarfile.TarInfo("package/link")
                info.type = tarfile.SYMTYPE
                info.linkname = "../../escape"
                stream.addfile(info)

            with self.assertRaisesRegex(ValueError, "unsafe archive"):
                release.read_tar_members(path)

    def test_linux_wheels_require_the_selected_manylinux_baseline(self) -> None:
        self.assertEqual(
            release.parse_wheel_target(
                Path("mikrio-0.1.0-cp312-abi3-manylinux_2_28_aarch64.whl"), "0.1.0"
            ),
            "linux-arm64",
        )

        for platform in ("linux_aarch64", "manylinux_2_39_aarch64", "any"):
            with self.assertRaises(ValueError):
                release.parse_wheel_target(
                    Path(f"mikrio-0.1.0-cp312-abi3-{platform}.whl"), "0.1.0"
                )

    def test_architecture_is_checked_from_native_bytes(self) -> None:
        payload = bytearray(64)
        payload[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", payload, 18, 183)
        release.verify_native_architecture(bytes(payload), "linux-arm64")

        with self.assertRaisesRegex(ValueError, "architecture"):
            release.verify_native_architecture(bytes(payload), "linux-x86_64")

        with self.assertRaisesRegex(ValueError, "truncated"):
            release.verify_native_architecture(b"", "linux-arm64")


class ReleasePreflightTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "--quiet", "--initial-branch=main")

        self.git(
            "-c",
            "user.name=Release tests",
            "-c",
            "user.email=release@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Initial commit",
        )

        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.revision = self.git("rev-parse", "HEAD").strip()

    def git(self, *arguments: str) -> str:
        return subprocess.check_output(
            ["git", *arguments], cwd=self.root, encoding="utf-8"
        )

    def preflight(self, **changes: str) -> dict[str, str]:
        environment = {
            "GITHUB_REPOSITORY": release.REPOSITORY,
            "GITHUB_EVENT_NAME": "workflow_dispatch",
            "GITHUB_REF": "refs/heads/main",
            "GITHUB_SHA": self.revision,
            "RELEASE_MODE": "rehearsal",
            "RELEASE_BOOTSTRAP": "false",
        }

        with (
            patch.dict("os.environ", environment | changes),
            patch.object(release, "ROOT", self.root),
            patch.object(release, "check", return_value="0.1.0"),
            patch.object(release, "release_notes"),
            patch.object(release, "write_output") as output,
        ):
            release.preflight()
            return dict(call.args for call in output.call_args_list)

    def test_rehearsal_does_not_publish_or_use_bootstrap_credentials(self) -> None:
        self.assertEqual(
            self.preflight(),
            {"version": "0.1.0", "publish": "false", "bootstrap": "false"},
        )

    def test_publish_supports_trusted_and_bootstrap_authentication(self) -> None:
        for bootstrap in ("false", "true"):
            with self.subTest(bootstrap=bootstrap):
                self.assertEqual(
                    self.preflight(RELEASE_MODE="publish", RELEASE_BOOTSTRAP=bootstrap),
                    {"version": "0.1.0", "publish": "true", "bootstrap": bootstrap},
                )

    def test_only_manual_runs_from_the_repository_main_branch_are_accepted(
        self,
    ) -> None:
        for changed, message in (
            ({"GITHUB_REPOSITORY": "someone/mikrio"}, "unexpected release repository"),
            ({"GITHUB_EVENT_NAME": "push"}, "run the release workflow from main"),
            (
                {"GITHUB_EVENT_NAME": "pull_request"},
                "run the release workflow from main",
            ),
            (
                {"GITHUB_REF": "refs/heads/feature"},
                "run the release workflow from main",
            ),
            ({"GITHUB_REF": "refs/tags/v0.1.0"}, "run the release workflow from main"),
        ):
            with (
                self.subTest(changed=changed),
                self.assertRaisesRegex(ValueError, message),
            ):
                self.preflight(RELEASE_MODE="publish", **changed)

    def test_invalid_modes_and_bootstrap_rehearsals_are_rejected(self) -> None:
        for changed, message in (
            ({"RELEASE_MODE": "unknown"}, "invalid release mode"),
            ({"RELEASE_MODE": ""}, "invalid release mode"),
            ({"RELEASE_BOOTSTRAP": "yes"}, "invalid release mode or bootstrap option"),
            (
                {"RELEASE_BOOTSTRAP": "true"},
                "bootstrap authentication requires publish",
            ),
        ):
            with (
                self.subTest(changed=changed),
                self.assertRaisesRegex(ValueError, message),
            ):
                self.preflight(**changed)

    def test_checkout_must_match_the_workflow_commit(self) -> None:
        with self.assertRaisesRegex(ValueError, "workflow source"):
            self.preflight(RELEASE_MODE="publish", GITHUB_SHA="different")

    def test_source_must_belong_to_main(self) -> None:
        self.git(
            "-c",
            "user.name=Release tests",
            "-c",
            "user.email=release@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Unmerged change",
        )

        with self.assertRaises(subprocess.CalledProcessError):
            self.preflight(
                RELEASE_MODE="publish", GITHUB_SHA=self.git("rev-parse", "HEAD").strip()
            )

    def test_dirty_source_cannot_be_released(self) -> None:
        (self.root / "untracked").write_bytes(b"unreviewed")

        with self.assertRaisesRegex(ValueError, "clean checkout"):
            self.preflight(RELEASE_MODE="publish")

    def test_existing_tag_requires_resuming_the_original_run(self) -> None:
        self.git("tag", "v0.1.0")

        with self.assertRaisesRegex(ValueError, "resume the original run"):
            self.preflight(RELEASE_MODE="publish")

        self.assertEqual(self.preflight()["publish"], "false")

    def test_similar_tag_does_not_block_a_new_release(self) -> None:
        self.git("tag", "v0.1.00")

        self.assertEqual(self.preflight(RELEASE_MODE="publish")["publish"], "true")
