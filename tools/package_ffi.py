"""Create and inspect relocatable C archives from captured native build inputs."""

from __future__ import annotations

import gzip
import hashlib
import io
import json
import tarfile
from pathlib import Path
from typing import Any

from tools.artifacts import digest
from tools.native_runtime import RECEIPT, runtime_for
from tools.version import validate


def library_names(matrix: str) -> tuple[str, str]:
    """Return the ABI-versioned image and its ordinary linker name."""

    if matrix in {"macos-arm64", "macos-x86_64"}:
        return "libmikrio.1.dylib", "libmikrio.dylib"

    if matrix in {"linux-arm64", "linux-x86_64"}:
        return "libmikrio.so.1", "libmikrio.so"

    if matrix == "windows-x86_64":
        return "mikrio.dll", "mikrio.lib"

    raise ValueError("C distribution requires a qualified native platform packager")


def stage_link(directory: Path, matrix: str) -> None:
    image, link = library_names(matrix)

    if matrix == "windows-x86_64":
        raise ValueError("Windows requires a real import library bound to mikrio.dll")

    (directory / link).symlink_to(image)


def package(
    installed: Path, destination: Path, metadata: dict[str, Any]
) -> dict[str, Any]:
    """Package shipping files only, excluding test clients and detector assets."""
    version = metadata["version"]
    matrix = metadata["matrix"]

    validate(version)

    image, link = library_names(matrix)
    library = installed / "lib" / image
    runtime = runtime_for(library)

    names: tuple[str, ...] = (
        "LICENSE",
        "README.md",
        "THIRD_PARTY_LICENSES.txt",
        "include/mikrio.h",
        "lib/" + image,
        "lib/" + runtime.name,
        "lib/" + RECEIPT,
    )

    if matrix == "windows-x86_64":
        names += ("lib/" + link,)

    files = {}

    for name in names:
        path = installed / name

        if path.is_symlink() or not path.is_file():
            raise ValueError("missing or linked C package input")

        files[name] = digest(path)

    links = {}

    if matrix != "windows-x86_64":
        if (installed / "lib" / link).readlink().as_posix() != image:
            raise ValueError("C linker name does not select its ABI image")

        links["lib/" + link] = image

    provenance = metadata | {"files": files, "links": links}
    prefix = f"mikrio-c-{version}-{matrix}"
    archive_path = destination / (prefix + ".tar.gz")
    destination.mkdir(parents=True, exist_ok=True)

    # Canonical archive headers remove packaging timestamps and host ownership.
    # Native compiler output can still vary; only measured hashes identify it.
    with archive_path.open("xb") as output:
        with gzip.GzipFile(
            filename="", mode="wb", fileobj=output, mtime=0
        ) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for name in sorted(files):
                    path = installed / name
                    member = tarfile.TarInfo(prefix + "/" + name)
                    member.size = path.stat().st_size
                    member.mode = 0o644

                    with path.open("rb") as stream:
                        archive.addfile(member, stream)

                for name, target in links.items():
                    member = tarfile.TarInfo(prefix + "/" + name)
                    member.type = tarfile.SYMTYPE
                    member.linkname = target
                    member.mode = 0o777
                    archive.addfile(member)

                payload = (
                    json.dumps(provenance, sort_keys=True, indent=2) + "\n"
                ).encode()

                member = tarfile.TarInfo(prefix + "/mikrio-build.json")
                member.size = len(payload)
                member.mode = 0o644
                archive.addfile(member, io.BytesIO(payload))

    verify(archive_path, provenance)

    return {
        "path": str(archive_path),
        "sha256": digest(archive_path),
        "metadata": provenance,
    }


def verify(archive_path: Path, expected: dict[str, Any]) -> dict[str, Any]:
    """Require exact files and the platform's import library or relative ABI alias."""

    validate(expected["version"])

    prefix = f"mikrio-c-{expected['version']}-{expected['matrix']}"
    image, link = library_names(expected["matrix"])

    runtime = (
        "libmikrio_sqlite3.dylib"
        if expected["matrix"].startswith("macos-")
        else "libmikrio_sqlite3.so"
    )

    if expected["matrix"] == "windows-x86_64":
        runtime = "mikrio_sqlite3.dll"

    required_files = {
        "LICENSE",
        "README.md",
        "THIRD_PARTY_LICENSES.txt",
        "include/mikrio.h",
        "lib/" + image,
        "lib/" + runtime,
        "lib/" + RECEIPT,
    }

    links = {"lib/" + link: image}

    if expected["matrix"] == "windows-x86_64":
        required_files.add("lib/" + link)
        links = {}

    if set(expected["files"]) != required_files:
        raise ValueError("C archive inventory differs from its platform contract")

    if expected["links"] != links:
        raise ValueError("unexpected C library alias")

    required = set(expected["files"]) | set(expected["links"]) | {"mikrio-build.json"}
    contents = {}

    with tarfile.open(archive_path) as archive:
        members = archive.getmembers()

        if len(members) != len(required) or {member.name for member in members} != {
            prefix + "/" + name for name in required
        }:
            raise ValueError("C archive has missing, duplicate or unexpected members")

        for member in members:
            name = member.name.removeprefix(prefix + "/")

            if name in expected["links"]:
                if not member.issym() or member.linkname != expected["links"][name]:
                    raise ValueError("C archive linker alias escapes its ABI image")

                if member.mode != 0o777:
                    raise ValueError("incorrect C archive permissions")

                continue

            if not member.isfile() or member.linkname:
                raise ValueError("C archive contains a non-file entry")

            if member.mode != 0o644:
                raise ValueError("incorrect C archive permissions")

            stream = archive.extractfile(member)

            if stream is None:
                raise ValueError("C archive member is missing")

            with stream:
                if name == "mikrio-build.json":
                    if json.load(stream) != expected:
                        raise ValueError("C archive build provenance changed")
                else:
                    checksum = hashlib.sha256()

                    while chunk := stream.read(1024 * 1024):
                        checksum.update(chunk)

                    contents[name] = checksum.hexdigest()

        if contents != expected["files"]:
            raise ValueError("C archive differs from its captured build inputs")

        receipt_stream = archive.extractfile(prefix + "/lib/" + RECEIPT)

        if receipt_stream is None:
            raise ValueError("missing C archive companion receipt")

        with receipt_stream:
            receipt = json.load(receipt_stream)

        if receipt != {"library": runtime, "sha256": contents["lib/" + runtime]}:
            raise ValueError("C archive companion receipt differs")

    return contents


def unpack(archive_path: Path, destination: Path, expected: dict[str, Any]) -> None:
    """Extract a verified archive into a fresh owned consumer location."""
    verify(archive_path, expected)
    destination.mkdir()
    prefix = f"mikrio-c-{expected['version']}-{expected['matrix']}/"

    with tarfile.open(archive_path) as archive:
        for member in archive.getmembers():
            path = destination / member.name.removeprefix(prefix)
            path.parent.mkdir(parents=True, exist_ok=True)

            if member.issym():
                path.symlink_to(member.linkname)
                continue

            stream = archive.extractfile(member)

            if stream is None:
                raise ValueError("C archive member is missing")

            with stream, path.open("xb") as output:
                while chunk := stream.read(1024 * 1024):
                    output.write(chunk)
