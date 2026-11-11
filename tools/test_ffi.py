"""Compile independent C/C++ consumers against an extracted C distribution."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sqlite3
import sys
import tarfile
import tempfile
from contextlib import closing
from pathlib import Path

from tools.artifacts import ROOT
from tools.package_ffi import unpack
from tools.test_bindings import environment, run


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)

    parser.add_argument(
        "--go", help="Also test the cgo consumer using this Go executable"
    )

    arguments = parser.parse_args()

    with tarfile.open(arguments.archive) as archive:
        entries = [
            item
            for item in archive.getmembers()
            if item.name.endswith("/mikrio-build.json")
        ]

        if len(entries) != 1:
            raise ValueError("missing or ambiguous C build manifest")

        stream = archive.extractfile(entries[0])

        if stream is None:
            raise ValueError("missing C build manifest")

        with stream:
            metadata = json.load(stream)

    with tempfile.TemporaryDirectory(prefix="mikrio-c-tests-") as temporary:
        directory = Path(temporary).resolve()
        installed = directory / "installed"
        unpack(arguments.archive, installed, metadata)
        binaries = installed / "bin"
        binaries.mkdir()
        env = environment()
        is_windows = os.name == "nt"

        if is_windows:
            for library in (installed / "lib").glob("*.dll"):
                shutil.copy2(library, binaries / library.name)

        programs = {}

        for name in (
            "layout",
            "ownership",
            "prefix",
            "inputs",
            "owner_options",
            "multipart",
            "extended",
            "administration",
        ):
            is_cpp = name == "layout"
            source = ROOT / "tests/ffi" / (name + (".cpp" if is_cpp else ".c"))
            binary = binaries / (name + (".exe" if is_windows else ""))

            if is_windows:
                command = [
                    "cl.exe",
                    "/nologo",
                    "/W4",
                    "/WX",
                    "/std:c++20" if is_cpp else "/std:c11",
                    "/I" + str(installed / "include"),
                    str(source),
                    str(installed / "lib/mikrio.lib"),
                    "/Fe:" + str(binary),
                    "/link",
                    "ws2_32.lib",
                ]
            else:
                loader = (
                    "@loader_path/../lib"
                    if sys.platform == "darwin"
                    else "$ORIGIN/../lib"
                )

                command = [
                    "c++" if is_cpp else "cc",
                    "-std=c++20" if is_cpp else "-std=c11",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-pthread",
                    "-I",
                    str(installed / "include"),
                    str(source),
                    "-L",
                    str(installed / "lib"),
                    "-lmikrio",
                    "-Wl,-rpath," + loader,
                    "-o",
                    str(binary),
                ]

            run(command, directory, env)
            programs[name] = str(binary)

        run([programs["layout"]], installed, env)
        store = directory / "ownership-store"
        run([programs["ownership"], "--initialize", str(store)], installed, env)

        with closing(
            sqlite3.connect(f"file:{store / 'metadata.sqlite3'}?mode=ro", uri=True)
        ) as connection:
            owner = connection.execute(
                "SELECT owner_account_id FROM store_metadata WHERE singleton_id = 1"
            ).fetchone()[0]

        run([programs["prefix"], str(store)], installed, env)
        run([programs["ownership"], str(store), owner], installed, env)

        for name in ("inputs", "owner_options", "multipart"):
            run([programs[name], str(directory / (name + "-store"))], installed, env)

        run(
            [programs["extended"], str(directory / "extended-store"), "current"],
            installed,
            env,
        )

        run(
            [
                sys.executable,
                str(ROOT / "tests/ffi/administration.py"),
                programs["administration"],
                str(ROOT / "tests/contract/http_sigv4.py"),
                str(directory / "administration"),
            ],
            installed,
            env,
        )

        print("C/C++ consumer tests passed")

        if arguments.go:
            env["CGO_CFLAGS"] = "-I" + str(installed / "include")
            env["CGO_LDFLAGS"] = "-L" + str(installed / "lib") + " -lmikrio"

            if not is_windows:
                env["CGO_LDFLAGS"] += " -Wl,-rpath," + str(installed / "lib")

            run(
                [arguments.go, "test", "-mod=readonly", "-v", "./..."],
                ROOT / "tests/ffi/go",
                env,
            )


if __name__ == "__main__":
    main()
