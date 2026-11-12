"""Run consumer tests against locally built wheel or npm archives."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

from tools.artifacts import ROOT
from tools.build import run as build_run
from tools.native_runtime import retain_native, runtime_from_build
from tools.node_packages import targets


def environment() -> dict[str, str]:
    """Do not let a checkout import path or loader override select other bytes."""
    result = os.environ.copy()

    for name in list(result):
        if name.startswith(("DYLD_", "LD_")) or name in {
            "PYTHONPATH",
            "PYTHONHOME",
            "NODE_OPTIONS",
            "NODE_PATH",
            "PYTEST_ADDOPTS",
            "PYTEST_PLUGINS",
            "MYPYPATH",
        }:
            result.pop(name, None)

    result["PYTEST_DISABLE_PLUGIN_AUTOLOAD"] = "1"
    result.setdefault("npm_config_cache", str(ROOT / ".cache/npm"))
    result.setdefault("UV_CACHE_DIR", str(ROOT / ".cache/uv"))
    return result


def run(command: list[str], directory: Path, env: dict[str, str]) -> None:
    print("+ " + " ".join(command), flush=True)
    subprocess.run(command, cwd=directory, env=env, check=True)


def build_probes(kind: str, directory: Path, python: str) -> dict[str, Path]:
    os.environ["PYO3_PYTHON"] = python
    os.environ.setdefault("PYO3_BUILD_EXTENSION_MODULE", "1")

    output = build_run(
        [
            "cargo",
            "build",
            "-p",
            "mikrio-" + kind,
            "--examples",
            "--release",
            "--locked",
            "--message-format=json-render-diagnostics",
        ]
    )

    records = [json.loads(line) for line in output.splitlines() if line.startswith("{")]

    runtime = runtime_from_build(records)

    result = {}

    for item in records:
        if item.get("reason") != "compiler-artifact":
            continue

        for probe in ("stream", "diagnostics"):
            if item["target"]["name"] != f"{kind}_{probe}_probe":
                continue

            paths = [
                Path(name)
                for name in item["filenames"]
                if name.endswith((".so", ".dylib", ".dll"))
            ]

            if len(paths) != 1:
                raise ValueError("missing or ambiguous native probe")

            suffix = ".node" if kind == "node" else ".pyd" if os.name == "nt" else ".so"
            destination = directory / (f"_mikrio_{probe}_probe" + suffix)

            if probe in result:
                raise ValueError("duplicate native probe build output")

            retain_native(paths[0], destination, runtime=runtime)
            result[probe] = destination

    if set(result) != {"stream", "diagnostics"}:
        raise ValueError("missing native probes")

    return result


def run_python_tests(wheel: Path, uv: str, selected: list[str], probes: bool) -> None:
    with tempfile.TemporaryDirectory(prefix="mikrio-python-tests-") as temporary:
        directory = Path(temporary).resolve()
        env = environment()
        virtualenv = directory / "venv"
        run([uv, "venv", "--python", sys.executable, str(virtualenv)], directory, env)

        python = virtualenv / (
            "Scripts/python.exe" if os.name == "nt" else "bin/python"
        )

        run(
            [
                uv,
                "pip",
                "install",
                "--no-index",
                "--no-deps",
                "--python",
                str(python),
                str(wheel),
            ],
            directory,
            env,
        )

        test_directory = ROOT / "packages/python/tests"

        for source in test_directory.glob("*.py"):
            shutil.copy2(source, directory / source.name)

        shutil.copy2(
            ROOT / "tests/bindings/native_guard.py", directory / "native_guard.py"
        )

        tests = selected or sorted(
            path.name
            for path in test_directory.glob("test_*.py")
            if probes or path.name not in {"test_streams.py", "test_diagnostics.py"}
        )

        if not tests:
            raise ValueError("no Python consumer tests selected")

        if probes:
            packages = Path(
                subprocess.check_output(
                    [
                        str(python),
                        "-c",
                        "import sysconfig; print(sysconfig.get_path('platlib'))",
                    ],
                    encoding="utf-8",
                ).strip()
            )

            build_probes("python", packages, str(python))

        run(
            [str(python), "-m", "unittest", "-v", *tests],
            directory,
            env,
        )


def run_node_tests(
    package: Path, native: Path, node: str, npm: str, selected: list[str], probes: bool
) -> None:
    with tempfile.TemporaryDirectory(prefix="mikrio-node-tests-") as temporary:
        directory = Path(temporary).resolve()
        env = environment()

        (directory / "package.json").write_text(
            json.dumps(
                {"name": "mikrio-consumer-tests", "private": True, "type": "module"}
            )
            + "\n",
            encoding="utf-8",
            newline="\n",
        )

        run(
            [
                npm,
                "install",
                "--ignore-scripts",
                "--offline",
                "--no-audit",
                "--no-fund",
                "--omit=optional",
                str(package),
                str(native),
            ],
            directory,
            env,
        )

        source = directory / "src"
        source.mkdir()
        files = []

        for path in (ROOT / "packages/node/tests").iterdir():
            if path.suffix not in {".mts", ".cts"}:
                continue

            shutil.copy2(path, source / path.name)
            files.append("src/" + path.name)

        (directory / "tsconfig.json").write_text(
            json.dumps(
                {
                    "extends": str(ROOT / "packages/node/tsconfig.json"),
                    "compilerOptions": {
                        "rootDir": "src",
                        "outDir": ".",
                        "declaration": False,
                        "typeRoots": [str(ROOT / "node_modules/@types")],
                    },
                    "include": [],
                    "exclude": [],
                    "files": sorted(files),
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
            newline="\n",
        )

        run(
            [
                node,
                str(ROOT / "node_modules/typescript-compiler/bin/tsc"),
                "-p",
                "tsconfig.json",
            ],
            directory,
            env,
        )

        shutil.copy2(
            ROOT / "tests/bindings/native_guard.py", directory / "native_guard.py"
        )

        fixtures = directory / "fixtures"
        fixtures.mkdir()

        env["mikrio_test_directory"] = str(fixtures)
        env["mikrio_test_python"] = sys.executable

        if probes:
            for name, path in build_probes("node", directory, sys.executable).items():
                env[f"mikrio_{name}_probe"] = str(path)

        excluded = {
            "streams.test.mjs",
            "diagnostics.test.mjs",
        }

        tests = selected or sorted(
            path.name
            for path in directory.glob("*.test.mjs")
            if probes or path.name not in excluded
        )

        if not tests:
            raise ValueError("no Node consumer tests selected")

        run(
            [
                node,
                "--no-global-search-paths",
                "--test",
                "--test-concurrency=1",
                *tests,
            ],
            directory,
            env,
        )

        with tempfile.TemporaryDirectory(prefix="mikrio-package-controls-") as controls:
            run(
                [
                    node,
                    "--no-global-search-paths",
                    str(directory / "package-loading.mjs"),
                ],
                Path(controls),
                env,
            )


def run_attachment_tests(
    package: Path, native: Path, node: str, pnpm: str, selected: list[str]
) -> None:
    with tempfile.TemporaryDirectory(prefix="mikrio-attachments-") as temporary:
        directory = Path(temporary).resolve() / "consumer"
        env = environment()
        directory.mkdir()
        example = ROOT / "examples/node/attachments"
        manifest = json.loads((example / "package.json").read_text(encoding="utf-8"))
        manifest.pop("devDependencies", None)

        (directory / "package.json").write_text(
            json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n"
        )

        # Preserve the exact SDK graph without a new registry resolution.
        script = """
const fs = require('node:fs');
const yaml = require(process.argv[1]);
const documents = yaml.parseAllDocuments(fs.readFileSync(process.argv[2], 'utf8'));

if (documents.some(document => document.errors.length)) {
  throw Error('invalid workspace lock');
}

console.log(JSON.stringify(
  documents.map(document => document.toJS())
    .filter(document => document.importers?.['examples/node/attachments'])
));
"""

        documents = json.loads(
            subprocess.check_output(
                [
                    node,
                    "-e",
                    script,
                    str(ROOT / "node_modules/yaml"),
                    str(ROOT / "pnpm-lock.yaml"),
                ],
                encoding="utf-8",
            )
        )

        if len(documents) != 1:
            raise ValueError("missing or ambiguous attachment lock importer")

        locked = documents[0]
        dependencies = locked["importers"]["examples/node/attachments"]["dependencies"]

        locked_dependencies = {
            name: value["specifier"] for name, value in dependencies.items()
        }

        if locked_dependencies != manifest["dependencies"]:
            raise ValueError("attachment manifest differs from the locked dependencies")

        locked["importers"] = {".": {"dependencies": dependencies}}

        (directory / "pnpm-lock.yaml").write_text(
            json.dumps(locked, indent=2) + "\n", encoding="utf-8", newline="\n"
        )

        config = (ROOT / "pnpm-workspace.yaml").read_text(encoding="utf-8")

        (directory / "pnpm-workspace.yaml").write_text(
            "packages: []\n\n" + config[config.index("# Native archives") :],
            encoding="utf-8",
            newline="\n",
        )

        run(
            [
                pnpm,
                "install",
                "--offline",
                "--frozen-lockfile",
                "--ignore-scripts",
                "--no-runtime",
                "--store-dir",
                str(ROOT / ".cache/pnpm-store"),
                "--state-dir",
                str(ROOT / ".cache/pnpm-state"),
                "--config.cache-dir=" + str(ROOT / ".cache/pnpm-cache"),
            ],
            directory,
            env,
        )

        # Install the exact local native archives beside the locked SDK closure.
        package_names = {"mikrio"} | {target["name"] for target in targets()}

        for archive_path in (package, native):
            with tarfile.open(archive_path) as archive:
                member = archive.extractfile("package/package.json")

                if member is None:
                    raise ValueError("npm archive is missing its manifest")

                with member:
                    manifest = json.load(member)

                name = manifest["name"]

                if not isinstance(name, str) or name not in package_names:
                    raise ValueError("unexpected npm package name")

                destination = directory / "node_modules" / name
                destination.mkdir(parents=True)

                with tempfile.TemporaryDirectory(dir=directory) as unpacked:
                    archive.extractall(unpacked, filter="data")

                    shutil.copytree(
                        Path(unpacked) / "package", destination, dirs_exist_ok=True
                    )

        for name in ("src", "tests"):
            shutil.copytree(example / name, directory / name)

        config_data = json.loads(
            (example / "tsconfig.json").read_text(encoding="utf-8")
        )

        config_data["compilerOptions"]["typeRoots"] = [
            str(ROOT / "node_modules/@types")
        ]

        (directory / "tsconfig.json").write_text(
            json.dumps(config_data, indent=2) + "\n", encoding="utf-8", newline="\n"
        )

        run(
            [
                node,
                str(ROOT / "node_modules/typescript-compiler/bin/tsc"),
                "-p",
                "tsconfig.json",
            ],
            directory,
            env,
        )

        shutil.copytree(example / "public", directory / "public")

        shutil.copy2(
            example / "tests/sdk-listener-policy.json",
            directory / "dist/tests/sdk-listener-policy.json",
        )

        tests = selected or sorted(
            path.name for path in (directory / "dist/tests").glob("*.test.mjs")
        )

        if not tests:
            raise ValueError("no attachment consumer tests selected")

        run(
            [
                node,
                "--no-global-search-paths",
                "--test",
                "--test-concurrency=1",
                *["dist/tests/" + name for name in tests],
            ],
            directory,
            env,
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("python", "node", "attachments"))
    parser.add_argument("package", type=Path)

    parser.add_argument(
        "--native", type=Path, help="Node platform package built from the same revision"
    )

    parser.add_argument("--uv", default="uv")
    parser.add_argument("--node", default="node")
    parser.add_argument("--npm", default="npm")
    parser.add_argument("--pnpm", default="pnpm")
    parser.add_argument("--test", action="append", default=[])

    parser.add_argument(
        "--probes",
        action="store_true",
        help="Also build and exercise native stream and diagnostic test extensions",
    )

    arguments = parser.parse_args()

    if arguments.kind == "python":
        run_python_tests(
            arguments.package.resolve(strict=True),
            arguments.uv,
            arguments.test,
            arguments.probes,
        )

        return

    if arguments.native is None:
        parser.error("Node tests require --native")

    if arguments.kind == "attachments":
        run_attachment_tests(
            arguments.package.resolve(strict=True),
            arguments.native.resolve(strict=True),
            arguments.node,
            arguments.pnpm,
            arguments.test,
        )

        return

    run_node_tests(
        arguments.package.resolve(strict=True),
        arguments.native.resolve(strict=True),
        arguments.node,
        arguments.npm,
        arguments.test,
        arguments.probes,
    )


if __name__ == "__main__":
    main()
