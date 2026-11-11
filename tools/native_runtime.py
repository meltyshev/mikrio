"""Retain a native image and the exact shared SQLite runtime it needs.

Cargo keeps native build outputs outside its executable directory. Installed
consumers use a companion library and a relative loader path instead. Ambiguous
build trees require a clean rebuild; choosing a runtime by modification time
would hide stale or differently instrumented inputs.
"""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path
from typing import Any

from tools.artifacts import digest

RUNTIMES = (
    "libmikrio_sqlite3.dylib",
    "libmikrio_sqlite3.so",
    "mikrio_sqlite3.dll",
)
RECEIPT = "mikrio-sqlite-runtime.json"
TARGETS = {
    "macos-arm64": "aarch64-apple-darwin",
    "macos-x86_64": "x86_64-apple-darwin",
    "linux-arm64": "aarch64-unknown-linux-gnu",
    "linux-x86_64": "x86_64-unknown-linux-gnu",
    "windows-x86_64": "x86_64-pc-windows-msvc",
}


def runtime_from_build(records: list[dict[str, Any]]) -> Path:
    """Select the runtime from the current Cargo invocation, never a stale cache."""

    candidates = [
        Path(record["out_dir"]) / name
        for record in records
        if record.get("reason") == "build-script-executed"
        and "mikrio-libsqlite3-sys" in record["package_id"]
        for name in RUNTIMES
        if (Path(record["out_dir"]) / name).is_file()
    ]

    if len(candidates) != 1:
        raise ValueError("missing or ambiguous SQLite runtime in Cargo build output")

    return candidates[0]


def check_receipt(path: Path, runtime: Path, checksum: str) -> None:
    """An existing retention record must still identify the companion bytes."""

    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise ValueError("shared SQLite receipt is linked or not a file")

    if not path.exists():
        return

    try:
        recorded = json.loads(path.read_text(encoding="utf-8"))
    except ValueError as error:
        raise ValueError("invalid shared SQLite receipt") from error

    if recorded != {"library": runtime.name, "sha256": checksum}:
        raise ValueError("shared SQLite runtime differs from its receipt")


def runtime_for(native: Path) -> Path:
    """Resolve a companion or a unique runtime from this Cargo profile only."""

    if native.is_symlink() or not native.is_file():
        raise ValueError("native image is missing or linked")

    adjacent = [native.parent / name for name in RUNTIMES]
    candidates = [path for path in adjacent if path.exists() or path.is_symlink()]

    if not candidates:
        profile = native.parent

        if profile.name in {"deps", "examples"}:
            profile = profile.parent
        elif native.match("*/build/*/*/out/*"):
            # The selected nightly also places executables in the crate output
            # directory: <profile>/build/<crate>/<hash>/out/<executable>.
            profile = native.parents[4]

        for name in RUNTIMES:
            # The selected stable and nightly compilers use different Cargo
            # build-output layouts. Both stay inside this dependency and profile.
            for pattern in (
                f"build/mikrio-libsqlite3-sys-*/out/{name}",
                f"build/mikrio-libsqlite3-sys/*/out/{name}",
            ):
                candidates.extend(sorted(profile.glob(pattern)))

    if not candidates or any(
        path.is_symlink() or not path.is_file() for path in candidates
    ):
        raise ValueError("shared SQLite runtime is missing or linked")

    identities = {(path.name, digest(path)) for path in candidates}

    if len(identities) != 1:
        raise ValueError(
            "ambiguous shared SQLite runtimes; rebuild in a clean target tree"
        )

    check_receipt(native.parent / RECEIPT, candidates[0], next(iter(identities))[1])
    return candidates[0]


def stage_runtime(native: Path, directory: Path) -> Path:
    """Copy the runtime without renaming its loader identity; retain its checksum."""
    runtime = runtime_for(native)
    checksum = digest(runtime)
    destination = directory / runtime.name
    receipt = directory / RECEIPT
    directory.mkdir(parents=True, exist_ok=True)

    for path in (destination, receipt):
        if path.is_symlink() or (path.exists() and not path.is_file()):
            raise ValueError("shared SQLite destination is linked or not a file")

    if destination.exists() and digest(destination) != checksum:
        raise ValueError("different shared SQLite runtime already staged")

    check_receipt(receipt, runtime, checksum)

    if not destination.exists():
        shutil.copy2(runtime, destination)

    if digest(runtime) != checksum or digest(destination) != checksum:
        raise ValueError("shared SQLite runtime changed during retention")

    receipt.write_text(
        json.dumps({"library": runtime.name, "sha256": checksum}, indent=2) + "\n",
        encoding="utf-8",
        newline="\n",
    )

    return destination


def retain_native(
    native: Path, destination: Path, *, runtime: Path | None = None
) -> Path:
    """Retain an independently executable image and its shared runtime together."""

    if native.is_symlink() or not native.is_file():
        raise ValueError("native image is missing or linked")

    if destination.is_symlink() or (destination.exists() and not destination.is_file()):
        raise ValueError("native image destination is linked or not a file")

    checksum = digest(native)

    if destination.exists() and digest(destination) != checksum:
        raise ValueError("different native image already retained")

    # A Cargo JSON build record can identify the exact build-script output when
    # the profile also contains unrelated feature or instrumentation variants.
    stage_runtime(runtime if runtime is not None else native, destination.parent)

    if not destination.exists():
        shutil.copy2(native, destination)

    if digest(native) != checksum or digest(destination) != checksum:
        raise ValueError("native image changed during retention")

    return destination


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("native", type=Path)
    parser.add_argument("destination", type=Path)
    arguments = parser.parse_args()
    retain_native(arguments.native, arguments.destination)


if __name__ == "__main__":
    main()
