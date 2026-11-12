"""Wheel auditing may change tags and records, but never package payloads."""

from __future__ import annotations

import tempfile
import unittest
import zipfile
from itertools import product
from pathlib import Path
from unittest.mock import patch

from tools import build_release
from tools.artifacts import digest


class ReleaseBuildTests(unittest.TestCase):
    def test_wheel_audit_accepts_directory_entries_and_rejects_payload_changes(
        self,
    ) -> None:
        cases = (
            "directories",
            "extension",
            "runtime",
            "renamed_runtime",
            "missing_extension",
            "added_library",
            "python",
            "nested_metadata_name",
        )

        for (target, architecture), platform, change in product(
            (("linux-arm64", "aarch64"), ("linux-x86_64", "x86_64")),
            ("linux", "manylinux_2_28"),
            cases,
        ):
            with (
                self.subTest(target=target, platform=platform, change=change),
                tempfile.TemporaryDirectory() as temporary,
            ):
                path = (
                    Path(temporary)
                    / f"mikrio-0.1.0-cp312-abi3-{platform}_{architecture}.whl"
                )

                self.check_wheel_audit(path, target, architecture, change)

    def check_wheel_audit(
        self, path: Path, target: str, architecture: str, change: str
    ) -> None:
        metadata_prefix = "mikrio-0.1.0.dist-info/"

        contents = {
            "mikrio/_native.abi3.so": b"native extension",
            "mikrio/libmikrio_sqlite3.so": b"shared runtime",
            "mikrio/__init__.py": b"Python wrapper",
            "mikrio/WHEEL": b"ordinary package file",
            metadata_prefix + "WHEEL": b"original tags",
            metadata_prefix + "RECORD": b"original record",
        }

        with zipfile.ZipFile(path, "w") as archive:
            for name, payload in contents.items():
                archive.writestr(name, payload)

        original_checksum = digest(path)

        def repair(command: list[str]) -> str:
            self.assertEqual(
                command[command.index("--plat") + 1], "manylinux_2_28_" + architecture
            )

            destination = Path(command[command.index("--wheel-dir") + 1])
            repaired = (
                destination
                / f"mikrio-0.1.0-cp312-abi3-manylinux_2_28_{architecture}.whl"
            )

            members = dict(contents)
            members[metadata_prefix + "WHEEL"] = b"audited tags"
            members[metadata_prefix + "RECORD"] = b"audited record"

            if change == "renamed_runtime":
                members["mikrio/libmikrio_sqlite3-renamed.so"] = members.pop(
                    "mikrio/libmikrio_sqlite3.so"
                )
            elif change == "missing_extension":
                del members["mikrio/_native.abi3.so"]
            elif change == "added_library":
                members["mikrio/libextra.so"] = b"extra library"
            elif change != "directories":
                name = {
                    "extension": "mikrio/_native.abi3.so",
                    "runtime": "mikrio/libmikrio_sqlite3.so",
                    "python": "mikrio/__init__.py",
                    "nested_metadata_name": "mikrio/WHEEL",
                }[change]

                members[name] = b"changed payload"

            with zipfile.ZipFile(repaired, "w") as archive:
                # auditwheel rebuilds directory entries when repacking a wheel.
                archive.writestr("mikrio/", b"")
                archive.writestr(metadata_prefix, b"")

                for name, payload in members.items():
                    archive.writestr(name, payload)

            return ""

        with patch.object(build_release, "run", side_effect=repair):
            if change != "directories":
                with self.assertRaisesRegex(ValueError, "changed wheel payload"):
                    build_release.audit_linux_wheel(path, target)

                self.assertEqual(digest(path), original_checksum)
                self.assertEqual(list(path.parent.iterdir()), [path])
                return

            result = build_release.audit_linux_wheel(path, target)

        self.assertEqual(list(path.parent.iterdir()), [result])

        with zipfile.ZipFile(result) as archive:
            for name, payload in contents.items():
                if name not in {metadata_prefix + "WHEEL", metadata_prefix + "RECORD"}:
                    self.assertEqual(archive.read(name), payload)

            self.assertEqual(archive.read(metadata_prefix + "WHEEL"), b"audited tags")
            self.assertEqual(
                archive.read(metadata_prefix + "RECORD"), b"audited record"
            )
