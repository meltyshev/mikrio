"""Version changes preserve unrelated content and restore failed updates."""

from __future__ import annotations

import json
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

from tools import version


class VersionTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()

        files = {
            "Cargo.toml": """[workspace]
members = ["crates/*"]
[workspace.package]
version = "0.1.0" # release
""",
            "crates/mikrio/Cargo.toml": """[package]
name = "mikrio"
version.workspace = true
[dependencies]
foreign = { package = "mikrio-vendor", version = "=0.1.0", path = "../../vendor/foreign" }
""",
            "crates/mikrio-python/Cargo.toml": """[package]
name = "mikrio-python"
version.workspace = true
[dependencies]
engine = { package = "mikrio", version = "=0.1.0", path = "../mikrio" } # exact
[target.'cfg(unix)'.dev-dependencies]
mikrio = { path = "../mikrio", version = "=0.1.0", default-features = false }
""",
            "Cargo.lock": """version = 4
[[package]]
name = "mikrio"
version = "0.1.0"
dependencies = ["mikrio-vendor"]
[[package]]
name = "mikrio-python"
version = "0.1.0"
dependencies = ["mikrio 0.1.0"]
[[package]]
name = "mikrio-vendor"
version = "0.1.0"
""",
            "packages/python/pyproject.toml": """[project]
name = "mikrio"
dynamic = ["version"]
[tool.maturin]
manifest-path = "../../crates/mikrio-python/Cargo.toml"
""",
            "packages/python/uv.lock": """version = 1
[[package]]
name = "mikrio"
source = { editable = "." }
[[package]]
name = "foreign"
version = "0.1.0"
source = { registry = "https://pypi.org/simple" }
""",
            "CHANGELOG.md": "# Changelog\n\n## 0.1.0\n\nInitial release.\n",
            "docs/storage.md": "0.1.0 creates storage format 1.\n",
            "vendor/foreign/Cargo.toml": '[package]\nname = "mikrio-vendor"\nversion = "0.1.0"\n',
        }

        for name, text in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8", newline="\n")

        node = self.root / "packages/node"
        (node / "src").mkdir(parents=True)
        targets = [{"name": "@mikrio/node-darwin-arm64"}]

        (node / "src/targets.json").write_text(
            json.dumps(targets), encoding="utf-8", newline="\n"
        )

        (node / "package.json").write_text(
            json.dumps(
                {
                    "name": "mikrio",
                    "version": "0.1.0",
                    "optionalDependencies": {targets[0]["name"]: "0.1.0"},
                    "dependencies": {"foreign": "0.1.0"},
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
            newline="\n",
        )

    def snapshot(self) -> dict[str, bytes]:
        return {
            str(path.relative_to(self.root)): path.read_bytes()
            for path in self.root.rglob("*")
            if path.is_file()
        }

    def test_patch_and_minor_updates_preserve_independent_versions(self) -> None:
        original = self.snapshot()

        for target in ("0.1.1", "0.2.0"):
            with self.subTest(target=target):
                changed = version.set_version(target, self.root)
                self.assertEqual(version.check(self.root), target)
                self.assertEqual(len(changed), 4)

                node = json.loads(
                    (self.root / "packages/node/package.json").read_text(
                        encoding="utf-8"
                    )
                )

                self.assertEqual(node["version"], target)
                self.assertEqual(set(node["optionalDependencies"].values()), {target})
                self.assertEqual(node["dependencies"], {"foreign": "0.1.0"})

                lock = tomllib.loads(
                    (self.root / "Cargo.lock").read_text(encoding="utf-8")
                )["package"]

                self.assertEqual(lock[1]["dependencies"], ["mikrio " + target])
                self.assertEqual(lock[2]["version"], "0.1.0")

                binding = (self.root / "crates/mikrio-python/Cargo.toml").read_text(
                    encoding="utf-8"
                )

                self.assertIn(f'version = "={target}"', binding)
                self.assertIn("# exact", binding)

                self.assertIn(
                    "# release", (self.root / "Cargo.toml").read_text(encoding="utf-8")
                )

                for name in (
                    "CHANGELOG.md",
                    "docs/storage.md",
                    "vendor/foreign/Cargo.toml",
                    "packages/python/uv.lock",
                    "crates/mikrio/Cargo.toml",
                ):
                    self.assertEqual((self.root / name).read_bytes(), original[name])

    def test_check_detects_each_kind_of_version_drift(self) -> None:
        original = self.snapshot()

        changes = {
            "packages/node/package.json": ('"version": "0.1.0"', '"version": "0.2.0"'),
            "crates/mikrio-python/Cargo.toml": (
                'version = "=0.1.0"',
                'version = "^0.1.0"',
            ),
            "Cargo.lock": ('version = "0.1.0"', 'version = "0.2.0"'),
            "packages/python/pyproject.toml": (
                'dynamic = ["version"]',
                'version = "0.1.0"',
            ),
            "packages/python/uv.lock": (
                'name = "mikrio"',
                'name = "mikrio"\nversion = "0.1.0"',
            ),
        }

        for name, (before, after) in changes.items():
            with self.subTest(name=name):
                path = self.root / name

                path.write_text(
                    path.read_text(encoding="utf-8").replace(before, after, 1),
                    encoding="utf-8",
                    newline="\n",
                )

                with self.assertRaises(ValueError):
                    version.check(self.root)

                path.write_bytes(original[name])

        node = self.root / "packages/node/package.json"
        package = json.loads(node.read_text(encoding="utf-8"))
        package["optionalDependencies"]["@mikrio/node-darwin-arm64"] = "0.2.0"
        node.write_text(json.dumps(package), encoding="utf-8", newline="\n")

        with self.assertRaises(ValueError):
            version.check(self.root)

    def test_invalid_versions_leave_every_file_untouched(self) -> None:
        before = self.snapshot()

        for value in (
            "01.2.3",
            "0.2",
            "v0.2.0",
            "0.2.0-rc.1",
            "0.2.0+build",
            "../0.2.0",
        ):
            with self.subTest(value=value), self.assertRaises(ValueError):
                version.set_version(value, self.root)

            self.assertEqual(self.snapshot(), before)

    def test_missing_lock_entry_fails_before_any_write(self) -> None:
        lock = self.root / "Cargo.lock"

        lock.write_text(
            lock.read_text(encoding="utf-8").replace(
                'name = "mikrio-python"', 'name = "other"'
            ),
            encoding="utf-8",
            newline="\n",
        )

        before = self.snapshot()

        with self.assertRaises(ValueError):
            version.set_version("0.2.0", self.root)

        self.assertEqual(self.snapshot(), before)

    def test_unhandled_dependency_layout_fails_before_any_write(self) -> None:
        path = self.root / "crates/mikrio-python/Cargo.toml"

        path.write_text(
            """[package]
name = "mikrio-python"
version.workspace = true
[dependencies.mikrio]
path = "../mikrio"
version = "=0.1.0"
""",
            encoding="utf-8",
            newline="\n",
        )

        before = self.snapshot()

        with self.assertRaisesRegex(ValueError, "inline exact version"):
            version.set_version("0.2.0", self.root)

        self.assertEqual(self.snapshot(), before)

    def test_registry_only_first_party_dependency_is_rejected(self) -> None:
        path = self.root / "crates/mikrio-python/Cargo.toml"

        path.write_text(
            path.read_text(encoding="utf-8").replace(', path = "../mikrio"', ""),
            encoding="utf-8",
            newline="\n",
        )

        with self.assertRaisesRegex(ValueError, "local workspace path"):
            version.check(self.root)

    def test_dependency_like_metadata_is_preserved(self) -> None:
        path = self.root / "crates/mikrio-python/Cargo.toml"

        metadata = """
[package.metadata]
example = { path = "../mikrio", version = "0.1.0" }
"""

        path.write_text(
            path.read_text(encoding="utf-8") + metadata, encoding="utf-8", newline="\n"
        )

        version.set_version("0.2.0", self.root)
        self.assertTrue(path.read_text(encoding="utf-8").endswith(metadata))
        self.assertEqual(version.check(self.root), "0.2.0")

    def test_dependency_like_multiline_string_fails_without_writes(self) -> None:
        path = self.root / "crates/mikrio-python/Cargo.toml"

        path.write_text(
            path.read_text(encoding="utf-8")
            + """
[package.metadata]
example = '''
[dependencies]
engine = { path = "../mikrio", version = "0.1.0" }
'''
""",
            encoding="utf-8",
            newline="\n",
        )

        before = self.snapshot()

        with self.assertRaisesRegex(ValueError, "unrelated TOML content"):
            version.set_version("0.2.0", self.root)

        self.assertEqual(self.snapshot(), before)

    def test_inherited_workspace_dependency_uses_the_new_exact_version(self) -> None:
        path = self.root / "Cargo.toml"

        path.write_text(
            path.read_text(encoding="utf-8")
            + """
[workspace.dependencies]
engine = { package = "mikrio", path = "crates/mikrio", version = "=0.1.0" }
""",
            encoding="utf-8",
            newline="\n",
        )

        path = self.root / "crates/mikrio-python/Cargo.toml"

        path.write_text(
            path.read_text(encoding="utf-8").replace(
                'engine = { package = "mikrio", version = "=0.1.0", path = "../mikrio" }',
                "engine = { workspace = true }",
            ),
            encoding="utf-8",
            newline="\n",
        )

        before = path.read_text(encoding="utf-8")
        version.set_version("0.2.0", self.root)
        manifest = tomllib.loads((self.root / "Cargo.toml").read_text(encoding="utf-8"))

        self.assertEqual(
            manifest["workspace"]["dependencies"]["engine"]["version"], "=0.2.0"
        )

        self.assertEqual(
            path.read_text(encoding="utf-8"), before.replace('"=0.1.0"', '"=0.2.0"')
        )

        self.assertEqual(version.check(self.root), "0.2.0")

    def test_registry_package_with_same_name_is_preserved(self) -> None:
        path = self.root / "Cargo.lock"

        external = """
[[package]]
name = "mikrio"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "external-checksum"
"""

        path.write_text(
            path.read_text(encoding="utf-8") + external, encoding="utf-8", newline="\n"
        )

        version.set_version("0.2.0", self.root)
        self.assertTrue(path.read_text(encoding="utf-8").endswith(external))

    def test_workspace_version_like_multiline_string_fails_without_writes(self) -> None:
        path = self.root / "Cargo.toml"

        path.write_text(
            """[workspace]
members = ["crates/*"]
package.version = "0.1.0"
[workspace.metadata]
example = '''
[workspace.package]
version = "0.0.0"
'''
""",
            encoding="utf-8",
            newline="\n",
        )

        before = self.snapshot()

        with self.assertRaisesRegex(ValueError, "unrelated TOML content"):
            version.set_version("0.1.0", self.root)

        self.assertEqual(self.snapshot(), before)

    def test_lock_reference_like_metadata_fails_without_writes(self) -> None:
        path = self.root / "Cargo.lock"

        path.write_text(
            path.read_text(encoding="utf-8")
            + '\n[metadata]\nexample = "mikrio 0.1.0"\n',
            encoding="utf-8",
            newline="\n",
        )

        before = self.snapshot()

        with self.assertRaisesRegex(ValueError, "unrelated metadata"):
            version.set_version("0.2.0", self.root)

        self.assertEqual(self.snapshot(), before)

    def test_same_version_is_a_noop(self) -> None:
        before = self.snapshot()
        self.assertEqual(version.set_version("0.1.0", self.root), [])
        self.assertEqual(self.snapshot(), before)

    def test_write_failure_or_interrupt_restores_already_written_files(self) -> None:
        before = self.snapshot()
        write_text = Path.write_text

        for failure in (OSError, KeyboardInterrupt):
            with self.subTest(failure=failure.__name__):

                def fail_at_lock(
                    path: Path,
                    text: str,
                    *,
                    encoding: str,
                    newline: str,
                    error_type: type[BaseException] = failure,
                ) -> int:
                    if path.name == "Cargo.lock":
                        path.write_bytes(b"partial write")
                        raise error_type("injected write failure")

                    return write_text(path, text, encoding=encoding, newline=newline)

                with patch.object(Path, "write_text", fail_at_lock):
                    with self.assertRaisesRegex(failure, "injected write failure"):
                        version.set_version("0.2.0", self.root)

                self.assertEqual(self.snapshot(), before)

    def test_failed_restore_does_not_skip_other_files(self) -> None:
        before = self.snapshot()
        write_bytes = Path.write_bytes

        def fail_root_restore(path: Path, data: bytes) -> int:
            if path == self.root / "Cargo.toml":
                raise OSError("injected restore failure")

            return write_bytes(path, data)

        with (
            patch.object(
                version, "check", side_effect=ValueError("injected check failure")
            ),
            patch.object(Path, "write_bytes", fail_root_restore),
            self.assertRaisesRegex(OSError, "Cargo.toml: injected restore failure"),
        ):
            version.set_version("0.2.0", self.root)

        after = self.snapshot()
        self.assertNotEqual(after.pop("Cargo.toml"), before.pop("Cargo.toml"))
        self.assertEqual(after, before)

    def test_failed_final_check_restores_every_file(self) -> None:
        before = self.snapshot()

        with (
            patch.object(
                version, "check", side_effect=ValueError("injected check failure")
            ),
            self.assertRaisesRegex(ValueError, "injected check failure"),
        ):
            version.set_version("0.2.0", self.root)

        self.assertEqual(self.snapshot(), before)
