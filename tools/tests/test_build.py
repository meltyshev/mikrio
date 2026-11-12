"""Builds preserve native exports, source inventory, and dependency order."""

from __future__ import annotations

import io
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from typing import Any
from unittest.mock import patch

from tools import build


class WindowsExportTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.header = self.root / "mikrio.h"

        self.header.write_text(
            "void mikrio_v1_first(void);\nvoid mikrio_v1_second(void);\n",
            encoding="utf-8",
            newline="\n",
        )

    def test_export_names_are_independent_of_alias_annotations_and_line_endings(
        self,
    ) -> None:
        for annotation in ("", " = mikrio_v1_first", " = implementation (void)"):
            for newline in ("\n", "\r\n"):
                with self.subTest(annotation=annotation, newline=newline):
                    output = newline.join(
                        [
                            "          2 number of functions",
                            "          2 number of names",
                            "    ordinal hint RVA      name",
                            "          1    0 00001000 mikrio_v1_first",
                            "          2    1 00001000 mikrio_v1_second" + annotation,
                            "",
                            "  Summary",
                            "        1000 .text",
                        ]
                    )

                    with patch.object(build, "run", return_value=output):
                        self.assertEqual(
                            build.read_windows_exports(
                                self.root / "mikrio.dll", self.header
                            ),
                            ["mikrio_v1_first", "mikrio_v1_second"],
                        )

    def test_incomplete_or_mismatched_export_tables_are_rejected(self) -> None:
        valid = (
            "          2 number of functions\n"
            "          2 number of names\n"
            "          1    0 00001000 mikrio_v1_first\n"
            "          2    1 00002000 mikrio_v1_second\n"
        )

        cases = {
            "missing": "          1    0 00001000 mikrio_v1_first\n",
            "extra": valid + "          3    2 00003000 unexpected_export\n",
            "duplicate": valid + "          3    2 00003000 mikrio_v1_first\n",
            "annotation_only": "          1    0 00001000 mikrio_v1_first = mikrio_v1_second\n",
            "unparsed_export": valid.replace("2 number", "3 number")
            + "          3    2          unexpected_export (forwarded to other.function)\n",
            "unnamed_export": valid.replace(
                "2 number of functions", "3 number of functions"
            ),
            "missing_counts": "\n".join(valid.splitlines()[2:]),
        }

        for name, output in cases.items():
            diagnostics = io.StringIO()

            with (
                self.subTest(name=name),
                patch.object(build, "run", return_value=output),
                redirect_stderr(diagnostics),
            ):
                with self.assertRaisesRegex(ValueError, "C export table differs"):
                    build.read_windows_exports(self.root / "mikrio.dll", self.header)

                self.assertEqual(diagnostics.getvalue(), output)


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
