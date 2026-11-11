"""Build and run an independent consumer of packaged or published Rust crates."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
from pathlib import Path

from tools.artifacts import ROOT, digest
from tools.native_runtime import retain_native, runtime_from_build
from tools.test_bindings import environment, run


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)

    parser.add_argument(
        "packages", type=Path, help="Directory produced by tools.build rust"
    )

    parser.add_argument("--offline", action="store_true")

    parser.add_argument(
        "--registry",
        action="store_true",
        help="Resolve the published crates without local patches",
    )

    arguments = parser.parse_args()

    order = json.loads(
        (arguments.packages / "publish-order.json").read_text(encoding="utf-8")
    )

    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))[
        "workspace"
    ]

    original_lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))

    with tempfile.TemporaryDirectory(prefix="mikrio-rust-consumer-") as temporary:
        directory = Path(temporary).resolve()
        sources = directory / "sources"
        sources.mkdir()
        patches = []

        for package in order:
            name, version = package["name"], package["version"]
            archive_path = arguments.packages / package["file"]

            with tarfile.open(archive_path) as archive:
                prefix = f"{name}-{version}/"

                if any(
                    not item.name.startswith(prefix) for item in archive.getmembers()
                ):
                    raise ValueError("crate archive contains an unexpected root")

                archive.extractall(sources, filter="data")

            source = sources / f"{name}-{version}"

            manifest = tomllib.loads(
                (source / "Cargo.toml").read_text(encoding="utf-8")
            )["package"]

            if (manifest["name"], manifest["version"]) != (name, version):
                raise ValueError("crate archive metadata differs from the package list")

            if not arguments.registry:
                patches.append(f"{name} = {{ path = {json.dumps(str(source))} }}")

        consumer = directory / "consumer"
        consumer.mkdir()
        (consumer / "src").mkdir()
        shutil.copy2(ROOT / "tests/rust_consumer.rs", consumer / "src/main.rs")
        support = consumer / "crates/mikrio/tests/support"
        support.mkdir(parents=True)

        shutil.copy2(
            ROOT / "crates/mikrio/tests/support/stores.rs", support / "stores.rs"
        )

        version = workspace["package"]["version"]
        tokio = workspace["dependencies"]["tokio"]["version"].lstrip("=")

        (consumer / "Cargo.toml").write_text(
            '[package]\nname = "mikrio-external-consumer"\nversion = "0.0.0"\n'
            'edition = "2024"\n\n[[bin]]\nname = "consumer"\npath = "src/main.rs"\n'
            '\n[features]\ndefault = ["http"]\nhttp = ["mikrio/http"]\n'
            f'\n[dependencies]\nmikrio = {{ version = "={version}", default-features = false }}\n'
            f'tokio = {{ version = "={tokio}", features = ["rt", "time"] }}\n'
            "\n[patch.crates-io]\n" + "\n".join(patches) + "\n",
            encoding="utf-8",
            newline="\n",
        )

        (consumer / "build.rs").write_text(
            """fn main() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path"),
        Ok("linux") => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN"),
        _ => {}
    }
}
""",
            encoding="utf-8",
            newline="\n",
        )

        shutil.copy2(ROOT / "Cargo.lock", consumer / "Cargo.lock")
        shutil.copy2(ROOT / "rust-toolchain.toml", consumer / "rust-toolchain.toml")
        env = environment()
        env.setdefault("CARGO_INCREMENTAL", "0")
        env.setdefault("CARGO_TARGET_DIR", str(ROOT / "target"))

        approved = {
            (item["name"], item["version"], item["source"], item["checksum"])
            for item in original_lock["package"]
            if item.get("source", "").startswith("registry+")
        }

        if arguments.registry:
            approved.update(
                (
                    item["name"],
                    item["version"],
                    "registry+https://github.com/rust-lang/crates.io-index",
                    digest(arguments.packages / item["file"]),
                )
                for item in order
            )

        output = build_consumer(consumer, env, approved, offline=arguments.offline)

        records = [
            json.loads(line) for line in output.splitlines() if line.startswith("{")
        ]

        binaries = [
            Path(item["executable"])
            for item in records
            if item.get("reason") == "compiler-artifact"
            and item.get("executable")
            and item["target"]["name"] == "consumer"
        ]

        runtime = runtime_from_build(records)

        if len(binaries) != 1:
            raise ValueError("missing or ambiguous consumer build output")

        executable = (
            directory
            / "installed"
            / ("consumer.exe" if os.name == "nt" else "consumer")
        )

        retain_native(binaries[0], executable, runtime=runtime)
        run([str(executable), str(directory / "store")], executable.parent, env)


def build_consumer(
    directory: Path,
    env: dict[str, str],
    approved: set[tuple[str, str, str, str]],
    *,
    offline: bool,
) -> str:
    # Resolve the copied lockfile before any dependency build script can execute.
    command = ["cargo", "metadata", "--format-version=1"]

    if offline:
        command.append("--offline")

    print("+ " + " ".join(command), flush=True)

    subprocess.run(
        command, cwd=directory, env=env, stdout=subprocess.DEVNULL, check=True
    )

    packages = tomllib.loads((directory / "Cargo.lock").read_text(encoding="utf-8"))[
        "package"
    ]

    for package in packages:
        if "source" not in package:
            continue

        identity = (
            package["name"],
            package["version"],
            package["source"],
            package.get("checksum", ""),
        )

        if identity not in approved:
            raise ValueError(
                "external consumer selected a dependency outside Cargo.lock"
            )

    command = [
        "cargo",
        "build",
        "--release",
        "--locked",
        "--message-format=json-render-diagnostics",
    ]

    if offline:
        command.append("--offline")

    print("+ " + " ".join(command), flush=True)
    return subprocess.check_output(command, cwd=directory, env=env, encoding="utf-8")


if __name__ == "__main__":
    main()
