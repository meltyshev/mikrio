"""Package and verify the CLI executable with its adjacent SQLite companion."""

from __future__ import annotations

import gzip
import hashlib
import io
import json
import tarfile
from pathlib import Path
from typing import Any

from tools.artifacts import digest
from tools.native_runtime import RECEIPT, TARGETS, runtime_for
from tools.version import validate


def binary_names(matrix: str) -> tuple[str, str]:
    if matrix not in TARGETS:
        raise ValueError("unknown CLI package target")

    if matrix == "windows-x86_64":
        return "mikrio.exe", "mikrio_sqlite3.dll"

    return "mikrio", (
        "libmikrio_sqlite3.dylib"
        if matrix.startswith("macos-")
        else "libmikrio_sqlite3.so"
    )


def prefix(metadata: dict[str, Any]) -> str:
    validate(metadata["version"])
    binary_names(metadata["matrix"])
    return f"mikrio-cli-{metadata['version']}-{metadata['matrix']}"


def package(
    installed: Path, destination: Path, metadata: dict[str, Any]
) -> dict[str, Any]:
    """Bind exact shipping files; refuse linked, missing or mismatched inputs."""
    executable, companion = binary_names(metadata["matrix"])
    files = {}

    for name in (
        executable,
        companion,
        RECEIPT,
        "LICENSE",
        "README.md",
        "THIRD_PARTY_LICENSES.txt",
    ):
        path = installed / name

        if path.is_symlink() or not path.is_file():
            raise ValueError("missing or linked CLI package input")

        files[name] = digest(path)

    if runtime_for(installed / executable).name != companion:
        raise ValueError("CLI companion does not match its target")

    provenance = metadata | {"files": files}
    root = prefix(provenance)
    destination.mkdir(parents=True, exist_ok=True)
    archive_path = destination / (root + ".tar.gz")
    payloads = {name: (installed / name).read_bytes() for name in files}

    payloads["mikrio-build.json"] = (
        json.dumps(provenance, sort_keys=True, indent=2) + "\n"
    ).encode()

    payloads["SHA256SUMS"] = "".join(
        f"{hashlib.sha256(payload).hexdigest()}  {name}\n"
        for name, payload in sorted(payloads.items())
    ).encode()

    with archive_path.open("xb") as output:
        with gzip.GzipFile(
            filename="", mode="wb", fileobj=output, mtime=0
        ) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for name, payload in sorted(payloads.items()):
                    member = tarfile.TarInfo(root + "/" + name)
                    member.size = len(payload)
                    member.mode = 0o755 if name == executable else 0o644
                    archive.addfile(member, io.BytesIO(payload))

    verify(archive_path, provenance)

    return {
        "path": str(archive_path),
        "sha256": digest(archive_path),
        "metadata": provenance,
    }


def verify(archive_path: Path, expected: dict[str, Any]) -> dict[str, str]:
    """Require a complete regular-file archive, exact modes, receipt and hashes."""
    root = prefix(expected)
    executable, companion = binary_names(expected["matrix"])

    required = {
        executable,
        companion,
        RECEIPT,
        "LICENSE",
        "README.md",
        "THIRD_PARTY_LICENSES.txt",
    }

    if set(expected.get("files", {})) != required:
        raise ValueError("incomplete CLI package file inventory")

    payloads = {}

    with tarfile.open(archive_path) as archive:
        for member in archive:
            name = member.name.removeprefix(root + "/")

            if (
                name not in required | {"mikrio-build.json", "SHA256SUMS"}
                or member.name != root + "/" + name
            ):
                raise ValueError("unexpected CLI archive member")

            if name in payloads or not member.isfile() or member.linkname:
                raise ValueError("duplicate or non-regular CLI archive member")

            if member.mode != (0o755 if name == executable else 0o644):
                raise ValueError("incorrect CLI archive permissions")

            stream = archive.extractfile(member)

            if stream is None:
                raise ValueError("unreadable CLI archive member")

            with stream:
                payloads[name] = stream.read()

    if set(payloads) != required | {"mikrio-build.json", "SHA256SUMS"}:
        raise ValueError("missing CLI archive member")

    if json.loads(payloads["mikrio-build.json"]) != expected:
        raise ValueError("CLI archive provenance differs")

    checksums = {
        name: hashlib.sha256(payload).hexdigest() for name, payload in payloads.items()
    }

    if any(checksums[name] != checksum for name, checksum in expected["files"].items()):
        raise ValueError("CLI archive member checksum differs")

    receipt = json.loads(payloads[RECEIPT])

    if receipt != {"library": companion, "sha256": checksums[companion]}:
        raise ValueError("CLI archive companion receipt differs")

    checksum_text = "".join(
        f"{checksum}  {name}\n"
        for name, checksum in sorted(checksums.items())
        if name != "SHA256SUMS"
    )

    if payloads["SHA256SUMS"] != checksum_text.encode():
        raise ValueError("CLI archive checksum manifest differs")

    return {root + "/" + name: checksum for name, checksum in checksums.items()}


def unpack(archive_path: Path, destination: Path, expected: dict[str, Any]) -> Path:
    """Extract checked regular files into a new directory without following links."""
    verify(archive_path, expected)
    destination.mkdir(parents=True, exist_ok=False)
    root = destination / prefix(expected)
    root.mkdir()

    with tarfile.open(archive_path) as archive:
        for member in archive:
            stream = archive.extractfile(member)

            if stream is None:
                raise ValueError("unreadable verified CLI member")

            path = root / Path(member.name).name

            with stream, path.open("xb") as output:
                output.write(stream.read())

            path.chmod(member.mode)

    for name, checksum in verify(archive_path, expected).items():
        if digest(destination / name) != checksum:
            raise ValueError("extracted CLI member checksum differs")

    return root
