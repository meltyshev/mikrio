"""Validate and assemble a complete release without publishing it."""

from __future__ import annotations

import argparse
import email.parser
import hashlib
import json
import os
import re
import shutil
import stat
import struct
import subprocess
import tarfile
import tomllib
import zipfile
from pathlib import Path, PurePosixPath

from tools.artifacts import ROOT, digest
from tools.build import rust_package_manifests
from tools.native_runtime import RECEIPT, TARGETS
from tools.node_packages import optional_dependencies, target_metadata
from tools.package_cli import binary_names
from tools.package_cli import verify as verify_cli
from tools.package_ffi import library_names
from tools.package_ffi import verify as verify_ffi
from tools.version import check

REPOSITORY = "meltyshev/mikrio"


def write_output(name: str, value: str) -> None:
    """Set a single-line Actions output, or display it during local preparation."""

    if "\n" in value or "\r" in value:
        raise ValueError("multiline workflow output")

    print(f"{name}={value}")

    if destination := os.environ.get("GITHUB_OUTPUT"):
        with Path(destination).open("a", encoding="utf-8", newline="\n") as stream:
            stream.write(f"{name}={value}\n")


def source_revision() -> str:
    return subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, encoding="utf-8"
    ).strip()


def release_notes(version: str) -> str:
    text = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    heading = rf"(?:\[{re.escape(version)}\]|{re.escape(version)})"

    match = re.search(
        rf"^## {heading}(?:[ \t]+[^\n]*)?\n(.*?)(?=^## |\Z)",
        text,
        re.M | re.S,
    )

    if match is None or not match[1].strip():
        raise ValueError("missing release changelog entry")

    return (
        match[1]
        .strip()
        .replace(
            "(SUPPORT.md)",
            f"(https://github.com/{REPOSITORY}/blob/v{version}/SUPPORT.md)",
        )
        + "\n"
    )


def preflight() -> None:
    version = check()
    release_notes(version)

    if subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT):
        raise ValueError("release preparation requires a clean checkout")

    if os.environ.get("GITHUB_REPOSITORY") != REPOSITORY:
        raise ValueError("unexpected release repository")

    event = os.environ.get("GITHUB_EVENT_NAME")
    ref = os.environ.get("GITHUB_REF")
    should_publish = event == "push" and ref == f"refs/tags/v{version}"
    is_rehearsal = event == "workflow_dispatch" and ref == "refs/heads/main"

    if not should_publish and not is_rehearsal:
        raise ValueError("use a matching version tag, or rehearse from main")

    subprocess.run(
        ["git", "merge-base", "--is-ancestor", "HEAD", "origin/main"],
        cwd=ROOT,
        check=True,
    )

    write_output("version", version)
    write_output("publish", str(should_publish).lower())


def is_safe_member_name(name: str) -> bool:
    path = PurePosixPath(name)

    return (
        not path.is_absolute()
        and ".." not in path.parts
        and "\\" not in name
        and name.rstrip("/") == str(path)
    )


def read_tar_members(path: Path, *, allow_links: bool = False) -> dict[str, bytes]:
    """Read regular members without extracting paths or accepting duplicate names."""
    contents = {}

    with tarfile.open(path) as archive:
        seen = set()

        for member in archive:
            if not is_safe_member_name(member.name) or member.name in seen:
                raise ValueError(f"unsafe archive member: {path.name}: {member.name}")

            is_allowed_type = (
                member.isfile() or member.isdir() or (allow_links and member.issym())
            )

            if not is_allowed_type:
                raise ValueError(f"unsafe archive member: {path.name}: {member.name}")

            seen.add(member.name)

            if member.isdir() or member.issym():
                continue

            stream = archive.extractfile(member)

            if stream is None:
                raise ValueError("unreadable archive member")

            with stream:
                contents[member.name] = stream.read()

    return contents


def rust_publish_order() -> list[dict[str, str]]:
    version = check()
    packages = []

    for path in rust_package_manifests():
        package = tomllib.loads(path.read_text(encoding="utf-8"))["package"]
        package_version = package["version"]

        if isinstance(package_version, dict):
            package_version = version

        packages.append(
            {
                "name": package["name"],
                "version": package_version,
                "file": f"{package['name']}-{package_version}.crate",
            }
        )

    return packages


def parse_wheel_target(path: Path, version: str) -> str:
    # abi3-py312 is part of the public Python build contract.
    match = re.fullmatch(
        rf"mikrio-{re.escape(version)}-cp312-abi3-(.+)\.whl", path.name
    )

    if match is None:
        raise ValueError(f"unexpected wheel identity: {path.name}")

    platform = match[1]

    if platform == "win_amd64":
        return "windows-x86_64"

    if platform == "manylinux_2_28_aarch64":
        return "linux-arm64"

    if platform == "manylinux_2_28_x86_64":
        return "linux-x86_64"

    if re.fullmatch(r"macosx_\d+_\d+_arm64", platform):
        return "macos-arm64"

    if re.fullmatch(r"macosx_\d+_\d+_x86_64", platform):
        return "macos-x86_64"

    raise ValueError(f"unsupported release wheel platform: {platform}")


def verify_native_architecture(payload: bytes, target: str) -> None:
    """Require the actual shipping image to match its advertised architecture."""
    is_arm64 = target.endswith("arm64")

    if len(payload) < 64:
        raise ValueError("truncated native image")

    if target.startswith("linux-"):
        is_valid = payload[:6] == b"\x7fELF\x02\x01" and struct.unpack_from(
            "<H", payload, 18
        )[0] == (183 if is_arm64 else 62)
    elif target.startswith("macos-"):
        is_valid = payload[:4] == b"\xcf\xfa\xed\xfe" and struct.unpack_from(
            "<I", payload, 4
        )[0] == (0x100000C if is_arm64 else 0x1000007)
    else:
        offset = struct.unpack_from("<I", payload, 60)[0]

        is_valid = (
            payload[:2] == b"MZ"
            and offset + 6 <= len(payload)
            and payload[offset : offset + 4] == b"PE\0\0"
            and struct.unpack_from("<H", payload, offset + 4)[0] == 0x8664
        )

    if not is_valid:
        raise ValueError(f"native image architecture differs from {target}")


def inspect_package(path: Path, version: str, commit: str) -> dict[str, str]:
    record = {"file": path.name, "sha256": digest(path), "version": version}

    if path.suffix == ".whl":
        return record | inspect_wheel(path, version)

    # C archives have an ABI linker alias, validated by their existing verifier.
    contents = read_tar_members(path, allow_links=path.name.startswith("mikrio-c-"))

    if path.suffix == ".crate":
        return record | inspect_crate(path, contents)

    if path.suffix == ".tgz":
        return record | inspect_npm(path, contents, version)

    if path.name == f"mikrio-{version}.tar.gz":
        return record | inspect_sdist(contents, version)

    return record | inspect_native_archive(path, contents, version, commit)


def inspect_wheel(path: Path, version: str) -> dict[str, str]:
    target = parse_wheel_target(path, version)

    with zipfile.ZipFile(path) as archive:
        members = archive.namelist()
        metadata_path = f"mikrio-{version}.dist-info/METADATA"
        metadata = email.parser.BytesParser().parsebytes(archive.read(metadata_path))

        if metadata["Name"] != "mikrio" or metadata["Version"] != version:
            raise ValueError("wheel metadata does not match its filename")

        required = {
            "mikrio/__init__.py",
            "mikrio/__init__.pyi",
            "mikrio/py.typed",
            "mikrio/THIRD_PARTY_LICENSES.txt",
            f"mikrio-{version}.dist-info/licenses/LICENSE",
            f"mikrio-{version}.dist-info/WHEEL",
            f"mikrio-{version}.dist-info/RECORD",
        }

        if not required.issubset(members):
            raise ValueError("wheel is missing package declarations or license files")

        if len(members) != len(set(members)):
            raise ValueError("unsafe wheel member: duplicate name")

        if any(not is_safe_member_name(name) for name in members):
            raise ValueError("unsafe wheel member: invalid path")

        if any(stat.S_ISLNK(info.external_attr >> 16) for info in archive.infolist()):
            raise ValueError("unsafe wheel member: symbolic link")

        if f"mikrio/{binary_names(target)[1]}" not in members:
            raise ValueError("wheel is missing its shared SQLite runtime")

        native_members = [
            name for name in members if name.endswith((".so", ".dylib", ".dll", ".pyd"))
        ]

        if len(native_members) != 2:
            raise ValueError("unexpected wheel native library inventory")

        if not any(name.startswith("mikrio/_native") for name in native_members):
            raise ValueError("unexpected wheel native library inventory")

        for name in native_members:
            verify_native_architecture(archive.read(name), target)

    return {"kind": "pypi", "name": "mikrio", "target": target}


def inspect_crate(path: Path, contents: dict[str, bytes]) -> dict[str, str]:
    package = next(
        (package for package in rust_publish_order() if package["file"] == path.name),
        None,
    )

    if package is None:
        raise ValueError("unexpected Rust package")

    prefix = f"{package['name']}-{package['version']}/"

    if any(not name.startswith(prefix) for name in contents):
        raise ValueError("unexpected crate root")

    manifest = tomllib.loads(contents[prefix + "Cargo.toml"].decode())["package"]
    expected_identity = (package["name"], package["version"])

    if (manifest["name"], manifest["version"]) != expected_identity:
        raise ValueError("crate manifest differs from the release inventory")

    return package | {"kind": "crates"}


def inspect_npm(path: Path, contents: dict[str, bytes], version: str) -> dict[str, str]:
    manifest = json.loads(contents["package/package.json"])

    required = {
        "package/package.json",
        "package/LICENSE",
        "package/README.md",
        "package/THIRD_PARTY_LICENSES.txt",
    }

    if not required.issubset(contents):
        raise ValueError("npm package is missing its README or license files")

    if any(not name.startswith("package/") for name in contents):
        raise ValueError("npm package has an unexpected root")

    if manifest["version"] != version or manifest.get("scripts"):
        raise ValueError("unexpected npm version or lifecycle scripts")

    name = manifest["name"]
    expected_filename = name.replace("@", "").replace("/", "-") + f"-{version}.tgz"

    if path.name != expected_filename:
        raise ValueError("npm filename differs from its package identity")

    if name == "mikrio":
        if manifest["optionalDependencies"] != optional_dependencies(version):
            raise ValueError("npm native dependencies differ from the release")

        entries = {
            "index.cjs",
            "index.mjs",
            "index.d.cts",
            "index.d.mts",
            "targets.json",
        }

        if not {"package/dist/" + entry for entry in entries}.issubset(contents):
            raise ValueError("npm wrapper is missing entry points or declarations")

        return {"kind": "npm", "name": name}

    matching_targets = [
        target_metadata(target)
        for target in TARGETS
        if target_metadata(target)["name"] == name
    ]

    if len(matching_targets) != 1:
        raise ValueError("unexpected native npm package")

    node_target = matching_targets[0]

    matches_platform = manifest["os"] == [node_target["os"]] and manifest["cpu"] == [
        node_target["cpu"]
    ]

    if not matches_platform:
        raise ValueError("npm platform metadata differs from its name")

    expected_libc = [node_target["libc"]] if "libc" in node_target else None

    if manifest.get("libc") != expected_libc:
        raise ValueError("npm platform metadata differs from its name")

    required |= {
        "package/" + member
        for member in ("mikrio.node", node_target["runtime"], RECEIPT)
    }

    if set(contents) != required:
        raise ValueError("native npm package has missing or unexpected files")

    for member in ("mikrio.node", node_target["runtime"]):
        verify_native_architecture(contents["package/" + member], node_target["matrix"])

    receipt = json.loads(contents["package/" + RECEIPT])
    checksum = hashlib.sha256(contents["package/" + node_target["runtime"]]).hexdigest()

    if receipt != {"library": node_target["runtime"], "sha256": checksum}:
        raise ValueError("npm companion receipt differs")

    return {"kind": "npm", "name": name}


def inspect_sdist(contents: dict[str, bytes], version: str) -> dict[str, str]:
    metadata = email.parser.BytesParser().parsebytes(
        contents[f"mikrio-{version}/PKG-INFO"]
    )

    if metadata["Name"] != "mikrio" or metadata["Version"] != version:
        raise ValueError("sdist metadata differs from the release")

    return {"kind": "pypi", "name": "mikrio", "target": "source"}


def inspect_native_archive(
    path: Path, contents: dict[str, bytes], version: str, commit: str
) -> dict[str, str]:
    for kind, verify_archive in (("cli", verify_cli), ("c", verify_ffi)):
        for target, triple in TARGETS.items():
            root = f"mikrio-{kind}-{version}-{target}"

            if path.name != root + ".tar.gz":
                continue

            metadata = json.loads(contents[root + "/mikrio-build.json"])

            expected_identity = {
                "version": version,
                "revision": commit,
                "matrix": target,
                "target": triple,
            }

            if any(metadata[key] != value for key, value in expected_identity.items()):
                raise ValueError("native archive has a different source or target")

            verify_archive(path, metadata)

            image = (
                binary_names(target)[0]
                if kind == "cli"
                else "lib/" + library_names(target)[0]
            )

            companion = (
                binary_names(target)[1]
                if kind == "cli"
                else "lib/" + binary_names(target)[1]
            )

            for member in (image, companion):
                verify_native_architecture(contents[root + "/" + member], target)

            return {"kind": kind, "name": "mikrio", "target": target}

    raise ValueError(f"unexpected release file: {path.name}")


def validate_inventory(records: list[dict[str, str]]) -> None:
    expected = {("crates", package["name"], "") for package in rust_publish_order()} | {
        ("npm", name, "") for name in ("mikrio", *optional_dependencies(check()))
    }

    expected |= {("pypi", "mikrio", target) for target in (*TARGETS, "source")}

    expected |= {
        (kind, "mikrio", target) for kind in ("cli", "c") for target in TARGETS
    }

    actual = [
        (package["kind"], package["name"], package.get("target", ""))
        for package in records
    ]

    if len(actual) != len(set(actual)) or set(actual) != expected:
        raise ValueError(
            "release inventory is incomplete or contains duplicate packages"
        )


def assemble(source: Path, destination: Path) -> None:
    version, commit = check(), source_revision()
    destination.mkdir(parents=True, exist_ok=False)

    for path in sorted(source.glob("*/*")):
        if path.name in {"SHA256SUMS", "publish-order.json"}:
            continue

        if not path.is_file() or path.is_symlink():
            raise ValueError("unexpected release artifact directory or link")

        destination_path = destination / path.name

        if destination_path.exists():
            if path.name != f"mikrio-{version}.tgz":
                raise ValueError("conflicting artifacts from native builders")

            if digest(path) != digest(destination_path):
                raise ValueError("conflicting artifacts from native builders")

            continue

        shutil.copyfile(path, destination_path)

    records = [
        inspect_package(path, version, commit) for path in sorted(destination.iterdir())
    ]

    validate_inventory(records)

    manifest = {
        "schema": 1,
        "version": version,
        "revision": commit,
        "packages": records,
    }

    (destination / "release.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n"
    )

    (destination / "publish-order.json").write_text(
        json.dumps(rust_publish_order(), indent=2) + "\n",
        encoding="utf-8",
        newline="\n",
    )

    (destination / "release-notes.md").write_text(
        release_notes(version), encoding="utf-8", newline="\n"
    )

    (destination / "SHA256SUMS").write_text(
        "".join(
            f"{digest(path)}  {path.name}\n" for path in sorted(destination.iterdir())
        ),
        encoding="utf-8",
        newline="\n",
    )

    verify(destination)


def verify(directory: Path) -> list[dict[str, str]]:
    if (directory / "SHA256SUMS").is_symlink():
        raise ValueError("linked checksum inventory")

    manifest = json.loads((directory / "release.json").read_text(encoding="utf-8"))

    if (
        manifest["schema"] != 1
        or manifest["version"] != check()
        or manifest["revision"] != source_revision()
    ):
        raise ValueError("release identity differs from the checked-out source")

    expected_checksums = {}

    for line in (directory / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([a-f0-9]{64})  ([A-Za-z0-9_.-]+)", line)

        if match is None or match[2] in expected_checksums or match[2] == "SHA256SUMS":
            raise ValueError("invalid or duplicate checksum entry")

        expected_checksums[match[2]] = match[1]

    actual_files = {path.name for path in directory.iterdir()}

    if set(expected_checksums) | {"SHA256SUMS"} != actual_files:
        raise ValueError("release files differ from the checksum inventory")

    for name, checksum in expected_checksums.items():
        path = directory / name

        if path.is_symlink() or not path.is_file() or digest(path) != checksum:
            raise ValueError(f"release checksum mismatch: {name}")

    records: list[dict[str, str]] = manifest["packages"]

    manifest_files = {package["file"] for package in records} | {
        "release.json",
        "publish-order.json",
        "release-notes.md",
    }

    if manifest_files != set(expected_checksums):
        raise ValueError("release manifest differs from the checksum inventory")

    for package in records:
        inspected = inspect_package(
            directory / package["file"], manifest["version"], manifest["revision"]
        )

        if inspected != package:
            raise ValueError("release package metadata differs from its manifest")

    validate_inventory(records)

    if (
        json.loads((directory / "publish-order.json").read_text(encoding="utf-8"))
        != rust_publish_order()
    ):
        raise ValueError("unexpected Rust publication order")

    return records


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("check", "assemble", "verify"))
    parser.add_argument("directory", type=Path, nargs="?", default=ROOT / "release")
    parser.add_argument("--input", type=Path, default=ROOT / "incoming")
    arguments = parser.parse_args()

    if arguments.action == "check":
        preflight()
    elif arguments.action == "assemble":
        assemble(arguments.input, arguments.directory)
    else:
        verify(arguments.directory)


if __name__ == "__main__":
    main()
