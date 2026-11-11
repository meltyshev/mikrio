"""CLI archives retain exact executable bytes and reject unsafe extraction."""

from __future__ import annotations

import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from typing import Any

from tools.artifacts import digest
from tools.native_runtime import RECEIPT, TARGETS
from tools.package_cli import binary_names, package, unpack


class CliPackageTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def create_package(self, matrix: str) -> tuple[Path, dict[str, Any]]:
        stage = self.root / matrix
        stage.mkdir()
        executable, companion = binary_names(matrix)

        for name in (
            executable,
            companion,
            "LICENSE",
            "README.md",
            "THIRD_PARTY_LICENSES.txt",
        ):
            (stage / name).write_bytes(name.encode())

        (stage / RECEIPT).write_text(
            json.dumps({"library": companion, "sha256": digest(stage / companion)}),
            encoding="utf-8",
            newline="\n",
        )

        result = package(
            stage,
            self.root / "packages",
            {"version": "0.1.0", "matrix": matrix, "revision": "fixture"},
        )

        return Path(result["path"]), result["metadata"]

    def test_all_target_archives_relocate_complete_checked_files(self) -> None:
        for matrix in TARGETS:
            with self.subTest(matrix=matrix):
                archive, metadata = self.create_package(matrix)
                executable, _ = binary_names(matrix)

                installed = unpack(
                    archive, self.root / (matrix + "-extracted"), metadata
                )

                for name, checksum in metadata["files"].items():
                    self.assertEqual(digest(installed / name), checksum)

                self.assertEqual((installed / executable).stat().st_mode & 0o777, 0o755)
                self.assertEqual((installed / "README.md").read_bytes(), b"README.md")

                self.assertIn(
                    "mikrio-build.json",
                    (installed / "SHA256SUMS").read_text(encoding="utf-8"),
                )

    def test_tampered_archive_is_rejected_before_extraction(self) -> None:
        archive, metadata = self.create_package("linux-arm64")

        errors = {
            "missing": "missing CLI archive member",
            "changed": "member checksum differs",
            "duplicate": "duplicate or non-regular",
            "linked": "duplicate or non-regular",
            "escape": "unexpected CLI archive member",
            "mode": "incorrect CLI archive permissions",
        }

        for mutation, message in errors.items():
            with self.subTest(mutation=mutation):
                changed = self.root / "changed.tar.gz"

                with (
                    tarfile.open(archive) as original,
                    tarfile.open(changed, "w:gz") as output,
                ):
                    for member in original:
                        stream = original.extractfile(member)
                        assert stream is not None

                        with stream:
                            payload = stream.read()

                        selected = member.name.endswith("/libmikrio_sqlite3.so")

                        if selected and mutation == "missing":
                            continue

                        if selected and mutation == "changed":
                            payload = b"wrong runtime"
                            member.size = len(payload)

                        if selected and mutation == "linked":
                            member.type = tarfile.SYMTYPE
                            member.linkname = "../../outside"
                            member.size = 0

                        if selected and mutation == "escape":
                            member.name = "../../outside"

                        if selected and mutation == "mode":
                            member.mode = 0o777

                        output.addfile(member, io.BytesIO(payload))

                        if selected and mutation == "duplicate":
                            output.addfile(member, io.BytesIO(payload))

                destination = self.root / "refused"

                with self.assertRaisesRegex(ValueError, message):
                    unpack(changed, destination, metadata)

                self.assertFalse(destination.exists())
