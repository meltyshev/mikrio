"""Release builds select matching archives and preserve audited wheel payloads."""

from __future__ import annotations

import os
import tempfile
import unittest
import zipfile
from itertools import product
from pathlib import Path
from unittest.mock import patch

from tools import build_release
from tools.artifacts import digest


class ReleaseBuildTests(unittest.TestCase):
    def test_native_build_selects_the_matching_scoped_archives(self) -> None:
        for target, suffix in (
            ("macos-arm64", "darwin-arm64"),
            ("macos-x86_64", "darwin-x64"),
            ("linux-arm64", "linux-arm64-gnu"),
            ("linux-x86_64", "linux-x64-gnu"),
            ("windows-x86_64", "win32-x64-msvc"),
        ):
            for change in ("none", "missing-public", "missing-native", "extra-native"):
                with (
                    self.subTest(target=target, change=change),
                    tempfile.TemporaryDirectory() as temporary,
                    patch.dict(os.environ),
                ):
                    directory = Path(temporary)
                    public = directory / "mikrio-node-0.1.0.tgz"
                    native = directory / f"mikrio-node-{suffix}-0.1.0.tgz"
                    wheel = directory / "test.whl"

                    for path in (public, native, wheel):
                        path.write_bytes(b"artifact")

                    if change == "missing-public":
                        public.unlink()
                    elif change == "missing-native":
                        native.unlink()
                    elif change == "extra-native":
                        (directory / "mikrio-node-unexpected-0.1.0.tgz").touch()

                    with (
                        patch.object(build_release, "check", return_value="0.1.0"),
                        patch.object(build_release, "invoke") as invoke,
                        patch.object(
                            build_release, "audit_linux_wheel", return_value=wheel
                        ),
                        patch.object(build_release, "test_cli"),
                    ):
                        if change != "none":
                            with self.assertRaisesRegex(
                                ValueError, "matching native npm"
                            ):
                                build_release.build_native(target, directory)

                            continue

                        build_release.build_native(target, directory)

                    node_tests = [
                        call.args
                        for call in invoke.call_args_list
                        if call.args[:2] == ("tools.test_bindings", "node")
                    ]

                    self.assertEqual(len(node_tests), 1)
                    self.assertEqual(node_tests[0][2], str(public))
                    self.assertEqual(node_tests[0][4], str(native))

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
