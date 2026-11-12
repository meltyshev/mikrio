"""Build distributable packages from the current workspace without publishing."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Any

from tools.artifacts import ROOT, digest
from tools.native_notices import generate
from tools.native_runtime import TARGETS, retain_native, runtime_from_build
from tools.node_packages import native_manifest, native_readme
from tools.package_cli import binary_names
from tools.package_cli import package as package_cli
from tools.package_ffi import library_names, stage_link
from tools.package_ffi import package as package_ffi
from tools.version import check, current


def run(command: list[str], *, cwd: Path = ROOT) -> str:
    """Run a build tool and retain its ordinary diagnostics on failure."""
    print("+ " + " ".join(command), flush=True)

    try:
        result = subprocess.run(
            command, cwd=cwd, encoding="utf-8", stdout=subprocess.PIPE, check=True
        )
    except subprocess.CalledProcessError as error:
        print(error.stdout, end="", file=sys.stderr)
        raise

    return result.stdout


def host_target() -> str:
    output = subprocess.check_output(["rustc", "-vV"], encoding="utf-8")

    return next(
        line.removeprefix("host: ")
        for line in output.splitlines()
        if line.startswith("host: ")
    )


def rust_package_manifests() -> list[Path]:
    """Order all publishable crates after their local dependencies."""

    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))[
        "workspace"
    ]

    paths = sorted(
        [*ROOT.glob("vendor/*/Cargo.toml"), *ROOT.glob("crates/*/Cargo.toml")]
    )

    manifests = {
        path: tomllib.loads(path.read_text(encoding="utf-8")) for path in paths
    }

    pending = {
        path
        for path in paths
        if manifests[path]["package"].get("publish", True) is not False
    }

    result: list[Path] = []

    while pending:
        ready = []

        for path in sorted(pending):
            manifest = manifests[path]
            tables = [manifest, *manifest.get("target", {}).values()]

            dependencies = {
                (path.parent / dependency["path"] / "Cargo.toml").resolve()
                for table in tables
                for kind in ("dependencies", "build-dependencies")
                for dependency in table.get(kind, {}).values()
                if isinstance(dependency, dict) and "path" in dependency
            }

            # Workspace aliases keep their selected maintained sources.
            for table in tables:
                for kind in ("dependencies", "build-dependencies"):
                    for name, dependency in table.get(kind, {}).items():
                        if not isinstance(dependency, dict) or not dependency.get(
                            "workspace"
                        ):
                            continue

                        inherited = workspace["dependencies"][name]

                        if isinstance(inherited, dict) and "path" in inherited:
                            dependencies.add(
                                (ROOT / inherited["path"] / "Cargo.toml").resolve()
                            )

            if not dependencies & pending:
                ready.append(path)

        if not ready:
            raise ValueError("cyclic local package dependencies")

        result.extend(ready)
        pending.difference_update(ready)

    return result


def package_rust(output: Path, offline: bool) -> None:
    """Create source archives; compilation and registry verification are separate."""
    workspace_version = current()
    order = []

    for path in rust_package_manifests():
        manifest = tomllib.loads(path.read_text(encoding="utf-8"))["package"]
        version = manifest["version"]

        if isinstance(version, dict):
            version = workspace_version

        command = [
            "cargo",
            "package",
            "--manifest-path",
            str(path),
            "--no-verify",
            "--exclude-lockfile",
            "--target-dir",
            str(output / "cargo"),
        ]

        if offline:
            command.append("--offline")

        run(command)
        archive = output / "cargo/package" / f"{manifest['name']}-{version}.crate"
        shutil.copy2(archive, output / archive.name)

        order.append(
            {"name": manifest["name"], "version": version, "file": archive.name}
        )

    (output / "publish-order.json").write_text(
        json.dumps(order, indent=2) + "\n", encoding="utf-8", newline="\n"
    )


def build_native(kind: str, target: str, offline: bool) -> tuple[Path, Path]:
    command = [
        "cargo",
        "build",
        "-p",
        "mikrio-" + kind,
        "--release",
        "--locked",
        "--message-format=json-render-diagnostics",
    ]

    if offline:
        command.append("--offline")

    if target != host_target():
        command.extend(["--target", target])

    output = run(command)
    records = [json.loads(line) for line in output.splitlines() if line.startswith("{")]
    runtime = runtime_from_build(records)
    candidates = []

    for item in records:
        if item.get("reason") != "compiler-artifact":
            continue

        if item["target"]["name"] != ("mikrio" if kind == "cli" else "mikrio_" + kind):
            continue

        if kind == "cli" and item.get("executable"):
            candidates.append(Path(item["executable"]))
        elif kind != "cli":
            candidates.extend(
                Path(name)
                for name in item["filenames"]
                if name.endswith((".dylib", ".so", ".dll"))
            )

    if len(candidates) != 1:
        raise ValueError("missing or ambiguous native build output")

    return candidates[0], runtime


def build_metadata(version: str, target: str, matrix: str) -> dict[str, Any]:
    revision = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, encoding="utf-8"
    ).strip()

    return {
        "version": version,
        "target": target,
        "matrix": matrix,
        "revision": revision,
    }


def write_package_documents(
    directory: Path, target: str, kind: str, offline: bool
) -> None:
    shutil.copy2(ROOT / "LICENSE", directory / "LICENSE")
    shutil.copy2(ROOT / f"crates/mikrio-{kind}/README.md", directory / "README.md")

    (directory / "THIRD_PARTY_LICENSES.txt").write_text(
        generate(target, kind, offline=offline), encoding="utf-8", newline="\n"
    )


def build_cli(
    output: Path, target: str, matrix: str, version: str, offline: bool
) -> None:
    native, runtime = build_native("cli", target, offline)

    with tempfile.TemporaryDirectory(prefix="mikrio-cli-") as temporary:
        stage = Path(temporary)
        retain_native(native, stage / binary_names(matrix)[0], runtime=runtime)
        write_package_documents(stage, target, "cli", offline)
        package_cli(stage, output, build_metadata(version, target, matrix))


def build_ffi(
    output: Path, target: str, matrix: str, version: str, offline: bool
) -> None:
    native, runtime = build_native("ffi", target, offline)

    with tempfile.TemporaryDirectory(prefix="mikrio-ffi-") as temporary:
        stage = Path(temporary)
        (stage / "lib").mkdir()
        (stage / "include").mkdir()
        image, _ = library_names(matrix)
        retain_native(native, stage / "lib" / image, runtime=runtime)

        shutil.copy2(
            ROOT / "crates/mikrio-ffi/include/mikrio.h", stage / "include/mikrio.h"
        )

        if matrix == "windows-x86_64":
            symbols = read_windows_exports(native, stage / "include/mikrio.h")
            definition = stage / "mikrio.def"

            definition.write_text(
                'LIBRARY "mikrio.dll"\nEXPORTS\n' + "\n".join(symbols) + "\n",
                encoding="utf-8",
                newline="\n",
            )

            run(
                [
                    "lib.exe",
                    "/nologo",
                    "/machine:x64",
                    "/def:" + str(definition),
                    "/out:" + str(stage / "lib/mikrio.lib"),
                ]
            )
        else:
            stage_link(stage / "lib", matrix)

        write_package_documents(stage, target, "ffi", offline)
        package_ffi(stage, output, build_metadata(version, target, matrix))


def read_windows_exports(native: Path, header: Path) -> list[str]:
    """Verify the DLL export names before generating its import library."""
    exported = run(["dumpbin.exe", "/nologo", "/exports", str(native)])

    # Read the name column, before any alias or debug-symbol annotation.
    symbols = re.findall(
        r"^[ \t]+\d+[ \t]+[0-9A-Fa-f]+[ \t]+[0-9A-Fa-f]+[ \t]+(\S+)",
        exported,
        re.M,
    )

    declared = set(
        re.findall(r"\b(mikrio_v1_\w+)\s*\(", header.read_text(encoding="utf-8"))
    )

    # Account for every entry, including unnamed or unparsed exports.
    reported_counts = [
        int(count)
        for count in re.findall(
            r"^[ \t]+(\d+)[ \t]+number of (?:functions|names)[ \t]*\r?$",
            exported,
            re.M,
        )
    ]

    if (
        not declared
        or set(symbols) != declared
        or len(symbols) != len(declared)
        or reported_counts != [len(declared), len(declared)]
    ):
        print(exported, end="", file=sys.stderr)
        missing = sorted(declared - set(symbols))
        unexpected = sorted(set(symbols) - declared)

        raise ValueError(
            "C export table differs from the public header: "
            f"missing={missing}, unexpected={unexpected}, "
            f"expected_count={len(declared)}, parsed_count={len(symbols)}, "
            f"reported_counts={reported_counts}"
        )

    return sorted(symbols)


def build_node(
    output: Path,
    target: str,
    matrix: str,
    version: str,
    offline: bool,
    node: str,
    npm: str,
) -> None:
    native, runtime = build_native("node", target, offline)
    package = ROOT / "packages/node"

    with tempfile.TemporaryDirectory(prefix="mikrio-node-") as temporary:
        stage = Path(temporary)
        public = stage / "public"
        implementation = stage / "native"
        public.mkdir()
        implementation.mkdir()
        manifest = json.loads((package / "package.json").read_text(encoding="utf-8"))

        for name in ("package.json", "LICENSE", "README.md"):
            shutil.copy2(package / name, public / name)

        run(
            [
                node,
                str(ROOT / "node_modules/typescript-compiler/bin/tsc"),
                "-p",
                "packages/node/tsconfig.json",
                "--outDir",
                str(public / "dist"),
                "--newLine",
                "lf",
            ]
        )

        (public / "THIRD_PARTY_LICENSES.txt").write_text(
            generate("supported", "node", offline=offline),
            encoding="utf-8",
            newline="\n",
        )

        for name in ("LICENSE", "THIRD_PARTY_LICENSES.txt"):
            shutil.copy2(public / name, implementation / name)

        (implementation / "package.json").write_text(
            json.dumps(native_manifest(version, matrix, manifest["engines"]), indent=2)
            + "\n",
            encoding="utf-8",
            newline="\n",
        )

        (implementation / "README.md").write_text(
            native_readme(matrix), encoding="utf-8", newline="\n"
        )

        retain_native(native, implementation / "mikrio.node", runtime=runtime)

        for directory in (public, implementation):
            result = run(
                [
                    npm,
                    "pack",
                    "--ignore-scripts",
                    "--pack-destination",
                    str(output),
                    "--json",
                ],
                cwd=directory,
            )

            for archive in json.loads(result):
                print(output / archive["filename"])


def build_python(
    output: Path, target: str, offline: bool, python: str, compatibility: str | None
) -> None:
    command = [
        sys.executable,
        "-m",
        "maturin",
        "build",
        "--release",
        "--locked",
        "--interpreter",
        python,
        "--out",
        str(output),
    ]

    if offline:
        command.append("--offline")

    if target != host_target():
        command.extend(["--target", target])

    if compatibility:
        command.extend(["--compatibility", compatibility])

    run(command, cwd=ROOT / "packages/python")


def build_sdist(output: Path, offline: bool) -> None:
    """Retain the locked Rust workspace beside the Python sources in the sdist.

    Cargo's per-crate sdist discovery resolves unpublished maintained crates
    through a registry. A Git source list in an isolated staging tree avoids
    that resolution while preserving the workspace's actual local paths.
    """

    files = (
        subprocess.check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            cwd=ROOT,
        )
        .decode()
        .split("\0")
    )

    with tempfile.TemporaryDirectory(prefix="mikrio-sdist-") as temporary:
        stage = Path(temporary)

        for name in files:
            if not name or not (ROOT / name).is_file():
                continue

            is_source = name in {
                "Cargo.toml",
                "Cargo.lock",
                "LICENSE",
                "rust-toolchain.toml",
            } or name.startswith(
                ("crates/", "vendor/", "packages/python/", "tests/contract/")
            )

            if not is_source:
                continue

            destination = stage / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, destination)

        manifest = (ROOT / "packages/python/pyproject.toml").read_text(encoding="utf-8")

        manifest = manifest.replace(
            'manifest-path = "../../crates/mikrio-python/Cargo.toml"',
            'manifest-path = "crates/mikrio-python/Cargo.toml"',
        )

        manifest = manifest.replace(
            'python-source = "."', 'python-source = "packages/python"'
        )

        manifest = manifest.replace(
            'readme = "README.md"', 'readme = "packages/python/README.md"'
        )

        manifest = manifest.replace(
            "[tool.maturin]\n",
            '[tool.maturin]\nsdist-generator = "git"\nlocked = true\n',
        )

        (stage / "pyproject.toml").write_text(manifest, encoding="utf-8", newline="\n")
        run(["git", "init", "--quiet"], cwd=stage)
        run(["git", "add", "--force", "."], cwd=stage)

        command = [
            sys.executable,
            "-m",
            "maturin",
            "sdist",
            "--manifest-path",
            "crates/mikrio-python/Cargo.toml",
            "--out",
            str(output),
        ]

        env = os.environ.copy()

        if offline:
            env["CARGO_NET_OFFLINE"] = "true"

        subprocess.run(command, cwd=stage, env=env, check=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)

    parser.add_argument(
        "kind", choices=("cli", "ffi", "node", "python", "sdist", "rust")
    )

    parser.add_argument("--target", choices=tuple(TARGETS.values()))
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--node", default="node")
    parser.add_argument("--npm", default="npm")

    parser.add_argument(
        "--compatibility",
        help="Validated wheel platform tag; Linux release builds require a manylinux tag",
    )

    arguments = parser.parse_args()

    try:
        version = check()
    except ValueError as error:
        parser.error(str(error))

    target = arguments.target or host_target()
    matrix = next((name for name, value in TARGETS.items() if value == target), None)

    if matrix is None:
        parser.error("unsupported native target")

    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    os.environ.setdefault("CARGO_INCREMENTAL", "0")
    os.environ.setdefault("npm_config_cache", str(ROOT / ".cache/npm"))

    if arguments.kind == "rust":
        package_rust(output, arguments.offline)
    elif arguments.kind == "sdist":
        build_sdist(output, arguments.offline)
    elif arguments.kind == "cli":
        build_cli(output, target, matrix, version, arguments.offline)
    elif arguments.kind == "ffi":
        build_ffi(output, target, matrix, version, arguments.offline)
    elif arguments.kind == "node":
        build_node(
            output,
            target,
            matrix,
            version,
            arguments.offline,
            arguments.node,
            arguments.npm,
        )
    else:
        build_python(
            output, target, arguments.offline, arguments.python, arguments.compatibility
        )

    artifacts = sorted(
        path
        for path in output.iterdir()
        if path.is_file() and path.name != "SHA256SUMS"
    )

    (output / "SHA256SUMS").write_text(
        "".join(f"{digest(path)}  {path.name}\n" for path in artifacts),
        encoding="utf-8",
        newline="\n",
    )


if __name__ == "__main__":
    main()
