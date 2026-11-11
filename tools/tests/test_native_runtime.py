"""Native retention must preserve the actual shared runtime and reject ambiguity."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from tools.artifacts import digest
from tools.native_runtime import (
    RECEIPT,
    retain_native,
    runtime_for,
    runtime_from_build,
    stage_runtime,
)


class NativeRuntimeTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.profile = self.root / "target/release"
        self.native = self.profile / "deps/consumer"
        self.native.parent.mkdir(parents=True)
        self.native.write_bytes(b"native image")

        self.runtime = (
            self.profile / "build/mikrio-libsqlite3-sys-one/out/libmikrio_sqlite3.so"
        )

        self.runtime.parent.mkdir(parents=True)
        self.runtime.write_bytes(b"ordinary shared SQLite")

    def test_cargo_records_require_one_current_runtime(self) -> None:
        record = {
            "reason": "build-script-executed",
            "package_id": "path+file:///workspace/vendor#mikrio-libsqlite3-sys@0.1.0",
            "out_dir": str(self.runtime.parent),
        }

        unrelated = record | {"package_id": "unrelated-dependency"}
        self.assertEqual(runtime_from_build([unrelated, record]), self.runtime)

        for records in ([unrelated], [record, record]):
            with (
                self.subTest(records=records),
                self.assertRaisesRegex(ValueError, "missing or ambiguous"),
            ):
                runtime_from_build(records)

    def test_retained_image_and_runtime_survive_build_cache_removal(self) -> None:
        destination = self.root / "installed/consumer"
        retain_native(self.native, destination)
        self.native.unlink()
        self.runtime.unlink()
        runtime = runtime_for(destination)
        self.assertEqual(runtime.read_bytes(), b"ordinary shared SQLite")
        self.assertEqual(destination.read_bytes(), b"native image")

        self.assertEqual(
            json.loads((destination.parent / RECEIPT).read_text(encoding="utf-8")),
            {"library": runtime.name, "sha256": digest(runtime)},
        )

    def test_differently_instrumented_build_outputs_are_ambiguous(self) -> None:
        other = self.profile / "build/mikrio-libsqlite3-sys-two/out" / self.runtime.name
        other.parent.mkdir(parents=True)
        other.write_bytes(b"instrumented shared SQLite")

        with self.assertRaisesRegex(ValueError, "ambiguous"):
            runtime_for(self.native)

    def test_identical_build_outputs_have_one_runtime_identity(self) -> None:
        other = self.profile / "build/mikrio-libsqlite3-sys-two/out" / self.runtime.name
        other.parent.mkdir(parents=True)
        other.write_bytes(self.runtime.read_bytes())
        self.assertEqual(digest(runtime_for(self.native)), digest(other))

    def test_cargo_selected_runtime_does_not_select_an_unrelated_cached_build(
        self,
    ) -> None:
        other = self.profile / "build/mikrio-libsqlite3-sys-two/out" / self.runtime.name
        other.parent.mkdir(parents=True)
        other.write_bytes(b"unrelated instrumented SQLite")
        destination = self.root / "installed/consumer"
        retain_native(self.native, destination, runtime=self.runtime)

        self.assertEqual(
            runtime_for(destination).read_bytes(), self.runtime.read_bytes()
        )

        with self.assertRaisesRegex(ValueError, "already staged"):
            retain_native(self.native, self.root / "installed/peer", runtime=other)

    def test_selected_nightly_layout_retains_runtime_and_rejects_mixed_policy(
        self,
    ) -> None:
        nightly = (
            self.profile
            / "build/mikrio-libsqlite3-sys/1339c537d7162777/out"
            / self.runtime.name
        )

        nightly.parent.mkdir(parents=True)
        self.runtime.rename(nightly)
        destination = self.root / "installed/consumer"
        retain_native(self.native, destination)

        self.assertEqual(
            runtime_for(destination).read_bytes(), b"ordinary shared SQLite"
        )

        self.runtime.write_bytes(b"different compiler policy")

        with self.assertRaisesRegex(ValueError, "ambiguous"):
            runtime_for(self.native)

    def test_missing_runtime_cannot_borrow_from_another_profile(self) -> None:
        self.runtime.unlink()

        other = (
            self.root
            / "target/debug/build/mikrio-libsqlite3-sys-one/out"
            / self.runtime.name
        )

        other.parent.mkdir(parents=True)
        other.write_bytes(b"wrong profile")

        with self.assertRaisesRegex(ValueError, "missing"):
            runtime_for(self.native)

    def test_selected_nightly_executable_uses_only_its_own_profile(self) -> None:
        executable = (
            self.profile
            / "build/mikrio-embed/24fc89165bc42a1d/out/mikrio_embed-24fc89165bc42a1d"
        )

        executable.parent.mkdir(parents=True)
        self.native.rename(executable)
        destination = self.root / "installed/consumer"
        retain_native(executable, destination)

        self.assertEqual(
            runtime_for(destination).read_bytes(), b"ordinary shared SQLite"
        )

        self.runtime.unlink()

        other = (
            self.root
            / "target/debug/build/mikrio-libsqlite3-sys-one/out"
            / self.runtime.name
        )

        other.parent.mkdir(parents=True)
        other.write_bytes(b"wrong profile")

        with self.assertRaisesRegex(ValueError, "missing"):
            runtime_for(executable)

    def test_symlinked_runtime_is_refused(self) -> None:
        other = self.runtime.with_suffix(".copy")
        self.runtime.rename(other)
        self.runtime.symlink_to(other)

        with self.assertRaisesRegex(ValueError, "linked"):
            runtime_for(self.native)

    def test_explicit_runtime_does_not_allow_a_linked_native_image(self) -> None:
        original = self.native.with_suffix(".original")
        self.native.rename(original)
        self.native.symlink_to(original)
        destination = self.root / "installed/consumer"

        with self.assertRaisesRegex(ValueError, "linked"):
            retain_native(self.native, destination, runtime=self.runtime)

        self.assertFalse(destination.parent.exists())

    def test_staging_cannot_replace_a_different_retained_runtime(self) -> None:
        installed = self.root / "installed"
        stage_runtime(self.native, installed)
        self.runtime.write_bytes(b"different")

        with self.assertRaisesRegex(ValueError, "already staged"):
            stage_runtime(self.native, installed)

        self.assertEqual(
            (installed / self.runtime.name).read_bytes(), b"ordinary shared SQLite"
        )

    def test_retention_cannot_overwrite_an_earlier_native_image(self) -> None:
        destination = self.root / "installed/consumer"
        retain_native(self.native, destination)
        self.native.write_bytes(b"different native image")

        with self.assertRaisesRegex(ValueError, "already retained"):
            retain_native(self.native, destination)

        self.assertEqual(destination.read_bytes(), b"native image")

    def test_changed_installed_runtime_cannot_receive_a_fresh_receipt(self) -> None:
        installed = self.root / "installed/consumer"
        retain_native(self.native, installed)
        (installed.parent / self.runtime.name).write_bytes(b"changed runtime")
        destination = self.root / "relocated/consumer"

        with self.assertRaisesRegex(ValueError, "receipt"):
            retain_native(installed, destination)

        self.assertFalse(destination.parent.exists())

    def test_existing_receipt_cannot_be_silently_rewritten(self) -> None:
        installed = self.root / "installed"
        stage_runtime(self.native, installed)
        receipt = installed / RECEIPT
        original = receipt.read_bytes()

        for content in (
            b"invalid JSON",
            b"[]",
            b'{"library": "other", "sha256": "changed"}',
        ):
            receipt.write_bytes(content)

            with (
                self.subTest(content=content),
                self.assertRaisesRegex(ValueError, "receipt"),
            ):
                stage_runtime(self.native, installed)

            self.assertEqual(receipt.read_bytes(), content)

        receipt.write_bytes(original)
        stage_runtime(self.native, installed)
        self.assertEqual(receipt.read_bytes(), original)

    def test_symlinked_destination_is_refused(self) -> None:
        installed = self.root / "installed"
        installed.mkdir()
        (installed / self.runtime.name).symlink_to(self.runtime)

        with self.assertRaisesRegex(ValueError, "linked"):
            stage_runtime(self.native, installed)

    def test_retaining_an_image_cannot_write_through_a_destination_link(self) -> None:
        original = self.root / "retained-image"
        original.write_bytes(b"previous evidence")
        destination = self.root / "installed/consumer"
        destination.parent.mkdir()
        destination.symlink_to(original)

        with self.assertRaisesRegex(ValueError, "linked"):
            retain_native(self.native, destination)

        self.assertEqual(original.read_bytes(), b"previous evidence")
        self.assertFalse((destination.parent / self.runtime.name).exists())

    def test_staging_a_runtime_cannot_write_through_a_receipt_link(self) -> None:
        original = self.root / "retained-receipt.json"
        original.write_bytes(b"previous evidence")
        installed = self.root / "installed"
        installed.mkdir()
        (installed / RECEIPT).symlink_to(original)

        with self.assertRaisesRegex(ValueError, "linked"):
            stage_runtime(self.native, installed)

        self.assertEqual(original.read_bytes(), b"previous evidence")
        self.assertFalse((installed / self.runtime.name).exists())

    def test_adjacent_runtime_is_preserved_when_copying_an_installed_image(
        self,
    ) -> None:
        adjacent = self.native.parent / self.runtime.name
        adjacent.write_bytes(b"installed runtime")
        self.assertEqual(runtime_for(self.native), adjacent)
        destination = self.root / "relocated/consumer"
        retain_native(self.native, destination)
        self.assertEqual(runtime_for(destination).read_bytes(), b"installed runtime")
