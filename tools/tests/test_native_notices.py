"""Native notices stay identical across hosts and local build activity."""

from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path, PureWindowsPath
from unittest.mock import patch

from tools import native_notices
from tools.artifacts import ROOT, digest


class NativeNoticesTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "dependency"
        (self.source / "src/spin").mkdir(parents=True)

        (self.root / "LICENSE").write_text(
            "Project license\n", encoding="utf-8", newline="\n"
        )

        (self.source / "src/spin/LICENSE").write_text(
            "Dependency license\n", encoding="utf-8", newline="\n"
        )

    def generate(self) -> str:
        metadata = {
            "packages": [
                {
                    "name": "dependency",
                    "version": "1.0.0",
                    "source": "registry+https://github.com/rust-lang/crates.io-index",
                    "manifest_path": str(self.source / "Cargo.toml"),
                    "license": "MIT",
                }
            ]
        }

        with (
            patch.object(native_notices, "ROOT", self.root),
            patch(
                "tools.native_notices.subprocess.check_output",
                side_effect=[json.dumps(metadata), "dependency v1.0.0\n"],
            ),
        ):
            return native_notices.generate("x86_64-pc-windows-msvc", "node")

    def test_nested_notice_paths_use_forward_slashes_on_windows(self) -> None:
        relative_to = Path.relative_to

        def windows_relative(path: Path, base: Path) -> PureWindowsPath:
            return PureWindowsPath(*relative_to(path, base).parts)

        with patch.object(Path, "relative_to", windows_relative):
            notices = self.generate()

        self.assertIn("--- src/spin/LICENSE ---\nDependency license\n", notices)

    def test_cargo_build_output_cannot_change_the_notice_inventory(self) -> None:
        expected = self.generate()
        generated = self.source / "target/package/dependency-1.0.0/LICENSE"
        generated.parent.mkdir(parents=True)

        generated.write_text(
            "Stale generated license\n", encoding="utf-8", newline="\n"
        )

        self.assertEqual(self.generate(), expected)


class ReviewedLicenseTests(unittest.TestCase):
    def test_reviewed_license_bytes_survive_windows_checkout(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            licenses = checkout / "vendor/licenses"
            shutil.copytree(ROOT / "vendor/licenses", licenses)
            attributes = ROOT / ".gitattributes"

            if attributes.is_file():
                shutil.copy2(attributes, checkout / attributes.name)

            sources = json.loads(
                (licenses / "sources.json").read_text(encoding="utf-8")
            )

            subprocess.run(["git", "init", "--quiet"], cwd=checkout, check=True)

            subprocess.run(
                ["git", "-c", "core.autocrlf=false", "add", "."],
                cwd=checkout,
                check=True,
            )

            shutil.rmtree(licenses)

            subprocess.run(
                ["git", "-c", "core.autocrlf=true", "checkout-index", "--all"],
                cwd=checkout,
                check=True,
            )

            for package, supplement in sources.items():
                with self.subTest(package=package):
                    self.assertEqual(
                        digest(checkout / supplement["path"]), supplement["sha256"]
                    )
