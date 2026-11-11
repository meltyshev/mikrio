"""Source builds preserve the Git inventory and local dependency order."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from tools import build


class SourceArchiveTests(unittest.TestCase):
    def test_rust_packages_follow_local_build_and_inherited_dependencies(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()

            (root / "Cargo.toml").write_text(
                '[workspace.dependencies]\nengine = { path = "crates/engine" }\n',
                encoding="utf-8",
                newline="\n",
            )

            manifests = {
                "cli": "[dependencies]\nengine = { workspace = true }\n",
                "engine": "[target.'cfg(unix)'.build-dependencies]\ncodegen = { path = \"../codegen\" }\n",
                "codegen": "",
                "private": "publish = false\n",
            }

            for name, dependencies in manifests.items():
                path = root / "crates" / name / "Cargo.toml"
                path.parent.mkdir(parents=True)

                path.write_text(
                    f'[package]\nname = "{name}"\n' + dependencies,
                    encoding="utf-8",
                    newline="\n",
                )

            with patch.object(build, "ROOT", root):
                self.assertEqual(
                    [path.parent.name for path in build.rust_package_manifests()],
                    ["codegen", "engine", "cli"],
                )

                path = root / "crates/codegen/Cargo.toml"

                path.write_text(
                    path.read_text(encoding="utf-8")
                    + '[dependencies]\ncli = { path = "../cli" }\n',
                    encoding="utf-8",
                    newline="\n",
                )

                with self.assertRaisesRegex(
                    ValueError, "cyclic local package dependencies"
                ):
                    build.rust_package_manifests()

    def test_sdist_stages_new_sources_and_excludes_deleted_or_ignored_files(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(["git", "init", "--quiet", str(root)], check=True)

            sources = {
                "Cargo.toml": "[workspace]\n",
                "packages/python/pyproject.toml": "[tool.maturin]\n",
                "crates/core/old.sql": "old schema",
                ".gitignore": "ignored.sql\n",
            }

            for name, content in sources.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8", newline="\n")

            subprocess.run(["git", "add", "."], cwd=root, check=True)
            (root / "crates/core/old.sql").unlink()

            (root / "crates/core/schema.sql").write_text(
                "current schema", encoding="utf-8", newline="\n"
            )

            (root / "crates/core/ignored.sql").write_text(
                "build output", encoding="utf-8", newline="\n"
            )

            (root / "notes.txt").write_text(
                "outside source directories", encoding="utf-8", newline="\n"
            )

            actual_run = subprocess.run
            staged = False

            def run(command: list[str], **kwargs: Any) -> Any:
                nonlocal staged

                if "maturin" not in command:
                    return actual_run(command, **kwargs)

                stage = Path(kwargs["cwd"])

                self.assertEqual(
                    (stage / "crates/core/schema.sql").read_text(encoding="utf-8"),
                    "current schema",
                )

                self.assertTrue((stage / "Cargo.toml").is_file())
                self.assertFalse((stage / "crates/core/old.sql").exists())
                self.assertFalse((stage / "crates/core/ignored.sql").exists())
                self.assertFalse((stage / "notes.txt").exists())
                staged = True
                return subprocess.CompletedProcess(command, 0)

            with patch.object(build, "ROOT", root), patch("subprocess.run", run):
                build.build_sdist(root / "dist", offline=True)

            self.assertTrue(staged)
