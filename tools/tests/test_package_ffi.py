"""C distributions bind archive contents, relative links and build provenance."""

from __future__ import annotations

import copy
import hashlib
import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path

from tools.artifacts import digest
from tools.native_runtime import RECEIPT
from tools.package_ffi import library_names, package, stage_link, unpack, verify


class FfiPackageTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.stage = self.root / "stage"

        for name in (
            "LICENSE",
            "README.md",
            "THIRD_PARTY_LICENSES.txt",
            "include/mikrio.h",
            "lib/libmikrio.1.dylib",
            "lib/libmikrio_sqlite3.dylib",
        ):
            path = self.stage / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(name.encode())

        runtime = self.stage / "lib/libmikrio_sqlite3.dylib"

        (self.stage / "lib" / RECEIPT).write_text(
            json.dumps({"library": runtime.name, "sha256": digest(runtime)}),
            encoding="utf-8",
            newline="\n",
        )

        stage_link(self.stage / "lib", "macos-arm64")

        metadata = {
            "version": "0.0.0",
            "matrix": "macos-arm64",
            "source": {"sha256": "fixture"},
        }

        self.result = package(self.stage, self.root / "archives", metadata)
        self.archive = Path(self.result["path"])
        self.metadata = self.result["metadata"]
        self.prefix = self.archive.name.removesuffix(".tar.gz")

    def rewrite(
        self,
        name: str,
        *,
        payload: bytes | None = None,
        link: str | None = None,
        omit: bool = False,
        duplicate: bool = False,
    ) -> Path:
        changed = self.root / "changed.tar.gz"

        with (
            tarfile.open(self.archive) as source,
            tarfile.open(changed, "w:gz") as destination,
        ):
            for original in source.getmembers():
                member = copy.copy(original)
                selected = member.name == self.prefix + "/" + name

                if selected and omit:
                    continue

                stream = source.extractfile(original) if original.isfile() else None
                content = stream.read() if stream is not None else None

                if stream is not None:
                    stream.close()

                if selected and payload is not None:
                    content = payload
                    member.size = len(content)

                if selected and link is not None:
                    member.linkname = link

                for _ in range(2 if selected and duplicate else 1):
                    destination.addfile(
                        member, io.BytesIO(content) if content is not None else None
                    )

        return changed

    def test_relocated_archive_contains_exact_inputs_and_relative_abi_alias(
        self,
    ) -> None:
        destination = self.root / "elsewhere"
        unpack(self.archive, destination, self.metadata)

        for name, checksum in self.metadata["files"].items():
            self.assertEqual(digest(destination / name), checksum)

        self.assertEqual((destination / "README.md").read_bytes(), b"README.md")

        self.assertEqual(
            (destination / "lib/libmikrio.dylib").readlink(), Path("libmikrio.1.dylib")
        )

        self.assertEqual(
            json.loads((destination / "mikrio-build.json").read_text(encoding="utf-8")),
            self.metadata,
        )

        self.assertEqual(
            library_names("linux-x86_64"), ("libmikrio.so.1", "libmikrio.so")
        )

    def test_windows_archive_requires_real_import_library_and_no_link_alias(
        self,
    ) -> None:
        stage = self.root / "windows"

        for name in (
            "LICENSE",
            "README.md",
            "THIRD_PARTY_LICENSES.txt",
            "include/mikrio.h",
            "lib/mikrio.dll",
            "lib/mikrio.lib",
            "lib/mikrio_sqlite3.dll",
        ):
            path = stage / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(name.encode())

        runtime = stage / "lib/mikrio_sqlite3.dll"

        (stage / "lib" / RECEIPT).write_text(
            json.dumps({"library": runtime.name, "sha256": digest(runtime)}),
            encoding="utf-8",
            newline="\n",
        )

        metadata = {"version": "0.0.0", "matrix": "windows-x86_64"}
        result = package(stage, self.root / "windows-archives", metadata)
        self.assertEqual(result["metadata"]["links"], {})
        destination = self.root / "windows-consumer"
        unpack(Path(result["path"]), destination, result["metadata"])
        self.assertFalse((destination / "lib/mikrio.lib").is_symlink())

        self.assertEqual(
            digest(destination / "lib/mikrio.lib"),
            result["metadata"]["files"]["lib/mikrio.lib"],
        )

        (stage / "lib/mikrio.lib").unlink()

        with self.assertRaisesRegex(ValueError, "missing or linked"):
            package(stage, self.root / "missing-import-library", metadata)

    def test_repacking_identical_inputs_has_identical_headers_and_bytes(self) -> None:
        metadata = {
            key: value
            for key, value in self.metadata.items()
            if key not in {"files", "links"}
        }

        repeated = package(self.stage, self.root / "repeat", metadata)
        self.assertEqual(repeated["sha256"], self.result["sha256"])

    def test_receipt_must_match_the_runtime_even_with_consistent_archive_hashes(
        self,
    ) -> None:
        altered = copy.deepcopy(self.metadata)
        receipt_name = "lib/" + RECEIPT
        payload = b'{"library": "libmikrio_sqlite3.dylib", "sha256": "stale"}'
        altered["files"][receipt_name] = hashlib.sha256(payload).hexdigest()

        replacements = {
            self.prefix + "/" + receipt_name: payload,
            self.prefix + "/mikrio-build.json": json.dumps(altered).encode(),
        }

        changed = self.root / "stale-receipt.tar.gz"

        with (
            tarfile.open(self.archive) as source,
            tarfile.open(changed, "w:gz") as destination,
        ):
            for member in source:
                if member.name in replacements:
                    content = replacements[member.name]
                    member.size = len(content)
                    destination.addfile(member, io.BytesIO(content))
                    continue

                stream = source.extractfile(member) if member.isfile() else None

                if stream is None:
                    destination.addfile(member)
                else:
                    with stream:
                        destination.addfile(member, stream)

        with self.assertRaisesRegex(ValueError, "companion receipt"):
            verify(changed, altered)

    def test_changed_missing_duplicate_or_escaping_archive_is_rejected(self) -> None:
        for name in self.metadata["files"]:
            for changes in (
                {"payload": b"tampered"},
                {"omit": True},
                {"duplicate": True},
            ):
                with self.subTest(name=name, changes=changes):
                    with self.assertRaises(ValueError):
                        verify(self.rewrite(name, **changes), self.metadata)

        for target in ("/tmp/foreign", "../../foreign", "libmikrio.2.dylib"):
            with self.subTest(target=target), self.assertRaises(ValueError):
                verify(self.rewrite("lib/libmikrio.dylib", link=target), self.metadata)

    def test_changed_build_provenance_or_inventory_is_rejected(self) -> None:
        altered = copy.deepcopy(self.metadata)
        altered["source"]["sha256"] = "another-source"

        with self.assertRaises(ValueError):
            verify(
                self.rewrite("mikrio-build.json", payload=json.dumps(altered).encode()),
                self.metadata,
            )

        altered = copy.deepcopy(self.metadata)
        altered["files"]["../../foreign"] = "unsafe"

        with self.assertRaises(ValueError):
            unpack(self.archive, self.root / "must-not-exist", altered)

        self.assertFalse((self.root / "must-not-exist").exists())

    def test_stale_companion_linked_image_and_missing_header_are_rejected(self) -> None:
        for name in ("lib/libmikrio_sqlite3.dylib", "include/mikrio.h"):
            path = self.stage / name
            original = path.read_bytes()

            if name.startswith("lib/"):
                path.write_bytes(b"changed")
            else:
                path.unlink()

            with self.subTest(name=name), self.assertRaises(ValueError):
                package(self.stage, self.root / "refused", self.metadata)

            path.write_bytes(original)

        image = self.stage / "lib/libmikrio.1.dylib"
        image.rename(image.with_name("outside"))
        image.symlink_to("outside")

        with self.assertRaises(ValueError):
            package(self.stage, self.root / "refused", self.metadata)
