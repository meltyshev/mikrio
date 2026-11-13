"""Build and exercise release archives using the existing package tools."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

from tools.artifacts import ROOT, digest
from tools.build import run
from tools.native_runtime import TARGETS
from tools.node_packages import PUBLIC_PACKAGE_NAME, archive_name, target_metadata
from tools.package_cli import binary_names, unpack
from tools.release import read_tar_members
from tools.test_bindings import environment
from tools.version import check


def invoke(*arguments: str) -> None:
    subprocess.run([sys.executable, "-m", *arguments], cwd=ROOT, check=True)


def audit_linux_wheel(path: Path, target: str) -> Path:
    """Audit the final wheel without allowing native companion rewrites."""
    platform = "manylinux_2_28_" + ("aarch64" if target == "linux-arm64" else "x86_64")
    distribution, version, _ = path.name.split("-", 2)

    metadata_members = {
        f"{distribution}-{version}.dist-info/{name}" for name in ("WHEEL", "RECORD")
    }

    with tempfile.TemporaryDirectory(prefix="mikrio-auditwheel-") as temporary:
        run(
            [
                "auditwheel",
                "repair",
                "--only-plat",
                "--plat",
                platform,
                "--wheel-dir",
                temporary,
                str(path),
            ]
        )

        wheels = list(Path(temporary).glob("*.whl"))

        if len(wheels) != 1:
            raise ValueError("auditwheel did not produce exactly one wheel")

        with zipfile.ZipFile(path) as original, zipfile.ZipFile(wheels[0]) as repaired:
            # Repacking adds directory entries; only file contents are payloads.
            before = {
                member.filename: original.read(member)
                for member in original.infolist()
                if not member.is_dir() and member.filename not in metadata_members
            }

            after = {
                member.filename: repaired.read(member)
                for member in repaired.infolist()
                if not member.is_dir() and member.filename not in metadata_members
            }

            if before != after:
                added = sorted(after.keys() - before.keys())
                removed = sorted(before.keys() - after.keys())

                changed = sorted(
                    name
                    for name in before.keys() & after.keys()
                    if before[name] != after[name]
                )

                raise ValueError(
                    "auditwheel changed wheel payload: "
                    f"added={added}, removed={removed}, changed={changed}"
                )

        destination = path.parent / wheels[0].name
        shutil.copyfile(wheels[0], destination)

        if destination != path:
            path.unlink()

    return destination


def build_native(target: str, destination: Path) -> None:
    version = check()

    npm = "npm.cmd" if os.name == "nt" else "npm"
    os.environ["PYO3_PYTHON"] = sys.executable
    os.environ["PYO3_BUILD_EXTENSION_MODULE"] = "1"

    for kind in ("cli", "ffi", "node", "python"):
        invoke(
            "tools.build",
            kind,
            "--target",
            TARGETS[target],
            "--output",
            str(destination),
            "--npm",
            npm,
        )

    wheels = list(destination.glob("*.whl"))

    if len(wheels) != 1:
        raise ValueError("expected exactly one native wheel")

    wheel = (
        audit_linux_wheel(wheels[0], target)
        if target.startswith("linux-")
        else wheels[0]
    )

    invoke("tools.test_bindings", "python", str(wheel))
    public_package = destination / archive_name(PUBLIC_PACKAGE_NAME, version)

    native_package = destination / archive_name(
        target_metadata(target)["name"], version
    )

    if set(destination.glob("*.tgz")) != {public_package, native_package}:
        raise ValueError("expected the public and matching native npm archives")

    invoke(
        "tools.test_bindings",
        "node",
        str(public_package),
        "--native",
        str(native_package),
        "--npm",
        npm,
    )

    invoke("tools.test_ffi", str(destination / f"mikrio-c-{version}-{target}.tar.gz"))
    archive = destination / f"mikrio-cli-{version}-{target}.tar.gz"
    test_cli(archive, target, version)

    (destination / "SHA256SUMS").write_text(
        "".join(
            f"{digest(path)}  {path.name}\n"
            for path in sorted(destination.iterdir())
            if path.name != "SHA256SUMS"
        ),
        encoding="utf-8",
        newline="\n",
    )


def test_cli(archive: Path, target: str, version: str) -> None:
    """Exercise the extracted executable without a loader-path override."""
    contents = read_tar_members(archive)

    metadata = json.loads(
        next(
            value
            for name, value in contents.items()
            if name.endswith("/mikrio-build.json")
        )
    )

    with tempfile.TemporaryDirectory(prefix="mikrio-cli-test-") as temporary:
        installed = unpack(archive, Path(temporary) / "installed", metadata)
        executable = installed / binary_names(target)[0]

        reported = subprocess.check_output(
            [str(executable), "--version"],
            cwd=temporary,
            env=environment(),
            encoding="utf-8",
        )

        if version not in reported.split():
            raise ValueError("installed CLI reports a different version")

        subprocess.run(
            [
                str(executable),
                "init",
                str(Path(temporary) / "store"),
                "--bucket",
                "release-check",
            ],
            cwd=temporary,
            env=environment(),
            check=True,
        )


def build_sources(destination: Path) -> None:
    invoke("tools.build", "rust", "--output", str(destination))

    # Existing maintained crate versions retain their original registry bytes.
    invoke("tools.publish", "reuse-crates", str(destination))
    invoke("tools.test_rust", str(destination))
    invoke("tools.build", "sdist", "--output", str(destination))
    sdist = destination / f"mikrio-{check()}.tar.gz"
    read_tar_members(sdist)

    with tempfile.TemporaryDirectory(prefix="mikrio-sdist-test-") as temporary:
        directory = Path(temporary)

        with tarfile.open(sdist) as archive:
            archive.extractall(directory, filter="data")

        source = directory / f"mikrio-{check()}"
        output = directory / "wheels"

        run(
            [
                sys.executable,
                "-m",
                "maturin",
                "build",
                "--release",
                "--locked",
                "--interpreter",
                sys.executable,
                "--out",
                str(output),
            ],
            cwd=source,
        )

        wheels = list(output.glob("*.whl"))

        if len(wheels) != 1:
            raise ValueError("sdist did not produce exactly one wheel")

        invoke("tools.test_bindings", "python", str(wheels[0]))

    # Cargo's intermediate files are not distribution artifacts.
    shutil.rmtree(destination / "cargo")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("native", "sources"))
    parser.add_argument("--target", choices=tuple(TARGETS))
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    arguments = parser.parse_args()
    destination = arguments.output.resolve()

    if destination.exists() and any(destination.iterdir()):
        parser.error("release builds require an empty output directory")

    os.environ.setdefault("CARGO_TARGET_DIR", str(ROOT / "target"))

    if arguments.kind == "sources":
        build_sources(destination)
    elif arguments.target:
        build_native(arguments.target, destination)
    else:
        parser.error("native builds require --target")


if __name__ == "__main__":
    main()
