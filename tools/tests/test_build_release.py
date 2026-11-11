"""Wheel auditing may change tags and records, but never native payloads."""

from __future__ import annotations

import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

from tools import build_release
from tools.artifacts import digest


class ReleaseBuildTests(unittest.TestCase):
    def test_wheel_audit_preserves_native_bytes_and_rejects_rewritten_companions(
        self,
    ) -> None:
        for rewrite_runtime in (False, True):
            with (
                self.subTest(rewrite_runtime=rewrite_runtime),
                tempfile.TemporaryDirectory() as temporary,
            ):
                path = Path(temporary) / "mikrio-0.1.0-cp312-abi3-linux_aarch64.whl"

                with zipfile.ZipFile(path, "w") as archive:
                    archive.writestr("mikrio/_native.abi3.so", b"native extension")
                    archive.writestr("mikrio/libmikrio_sqlite3.so", b"shared runtime")
                    archive.writestr("mikrio-0.1.0.dist-info/WHEEL", "original tags")
                    archive.writestr("mikrio-0.1.0.dist-info/RECORD", "original record")

                original_checksum = digest(path)

                def repair(
                    command: list[str],
                    source_path: Path = path,
                    should_rewrite: bool = rewrite_runtime,
                ) -> str:
                    destination = Path(command[command.index("--wheel-dir") + 1])

                    repaired = (
                        destination
                        / "mikrio-0.1.0-cp312-abi3-manylinux_2_28_aarch64.whl"
                    )

                    with (
                        zipfile.ZipFile(source_path) as source,
                        zipfile.ZipFile(repaired, "w") as output,
                    ):
                        for name in source.namelist():
                            payload = source.read(name)

                            if name.endswith(("/WHEEL", "/RECORD")):
                                payload = b"audited metadata"

                            if should_rewrite and name.endswith("libmikrio_sqlite3.so"):
                                payload = b"rewritten companion"

                            output.writestr(name, payload)

                    return ""

                with patch.object(build_release, "run", side_effect=repair):
                    if rewrite_runtime:
                        with self.assertRaisesRegex(ValueError, "changed native bytes"):
                            build_release.audit_linux_wheel(path, "linux-arm64")

                        self.assertEqual(digest(path), original_checksum)
                        continue

                    result = build_release.audit_linux_wheel(path, "linux-arm64")

                self.assertFalse(path.exists())

                with zipfile.ZipFile(result) as archive:
                    self.assertEqual(
                        archive.read("mikrio/_native.abi3.so"), b"native extension"
                    )

                    self.assertEqual(
                        archive.read("mikrio/libmikrio_sqlite3.so"), b"shared runtime"
                    )
