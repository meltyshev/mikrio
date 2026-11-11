"""Rust consumer compilation uses only dependencies admitted by Cargo.lock."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from tools import test_rust


class RustConsumerTests(unittest.TestCase):
    def test_unreviewed_dependency_fails_before_compilation(self) -> None:
        registry = "registry+https://github.com/rust-lang/crates.io-index"
        approved = {("dependency", "1.0.0", registry, "reviewed")}

        for version, source, checksum in (
            ("1.0.1", registry, "reviewed"),
            ("1.0.0", "git+https://example.invalid/dependency", "reviewed"),
            ("1.0.0", registry, "changed"),
        ):
            with self.subTest(version=version, source=source, checksum=checksum):
                with tempfile.TemporaryDirectory() as temporary:
                    directory = Path(temporary)

                    (directory / "Cargo.lock").write_text(
                        '[[package]]\nname = "dependency"\n'
                        f'version = "{version}"\nsource = "{source}"\n'
                        f'checksum = "{checksum}"\n',
                        encoding="utf-8",
                        newline="\n",
                    )

                    with (
                        patch("tools.test_rust.subprocess.run"),
                        patch(
                            "tools.test_rust.subprocess.check_output"
                        ) as compile_process,
                        self.assertRaisesRegex(ValueError, "outside Cargo.lock"),
                    ):
                        test_rust.build_consumer(directory, {}, approved, offline=True)

                    compile_process.assert_not_called()

    def test_reviewed_dependencies_build_with_the_validated_lockfile(self) -> None:
        registry = "registry+https://github.com/rust-lang/crates.io-index"
        approved = {("dependency", "1.0.0", registry, "reviewed")}

        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)

            (directory / "Cargo.lock").write_text(
                '[[package]]\nname = "consumer"\nversion = "0.0.0"\n'
                '[[package]]\nname = "dependency"\nversion = "1.0.0"\n'
                f'source = "{registry}"\nchecksum = "reviewed"\n',
                encoding="utf-8",
                newline="\n",
            )

            with (
                patch("tools.test_rust.subprocess.run"),
                patch(
                    "tools.test_rust.subprocess.check_output",
                    return_value="build output",
                ) as compile_process,
            ):
                output = test_rust.build_consumer(directory, {}, approved, offline=True)

            self.assertEqual(output, "build output")
            self.assertIn("--locked", compile_process.call_args.args[0])
            self.assertIn("--offline", compile_process.call_args.args[0])
