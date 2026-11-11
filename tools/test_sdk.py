"""Run the Python and JavaScript SDK suites against an owned local fixture."""

from __future__ import annotations

import argparse
import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

from tools.artifacts import ROOT
from tools.build import run
from tools.native_runtime import runtime_from_build, stage_runtime
from tools.test_bindings import environment


def build_fixture(listener: bool, offline: bool, release: bool) -> tuple[Path, Path]:
    command = [
        "cargo",
        "test",
        "-p",
        "mikrio" if listener else "mikrio-http",
        "--no-run",
        "--locked",
        "--message-format=json-render-diagnostics",
    ]

    command += ["--test", "listener_sdk"] if listener else ["--lib"]

    if offline:
        command.append("--offline")

    if release:
        command.append("--release")

    output = run(command)
    records = [json.loads(line) for line in output.splitlines() if line.startswith("{")]
    runtime = runtime_from_build(records)
    candidates = []

    for item in records:
        if item.get("reason") == "compiler-artifact" and item.get("executable"):
            candidates.append(Path(item["executable"]))

    if len(candidates) != 1:
        raise ValueError("missing or ambiguous SDK fixture executable")

    return candidates[0], runtime


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)

    parser.add_argument(
        "mode",
        choices=("integrity", "phases", "storage", "listener"),
        default="storage",
        nargs="?",
    )

    parser.add_argument("--node", default="node")
    parser.add_argument("--offline", action="store_true")

    parser.add_argument(
        "--release",
        action="store_true",
        help="Use an optimized fixture for large SDK transfer suites",
    )

    parser.add_argument(
        "--go", help="Also run Go SDK tests using this Go executable (listener only)"
    )

    parser.add_argument(
        "--suite",
        action="append",
        choices=("multipart", "post", "transfers"),
        default=[],
        help="Additional suite for the listener fixture; may be repeated",
    )

    arguments = parser.parse_args()

    if (arguments.suite or arguments.go) and arguments.mode != "listener":
        parser.error("additional suites require the listener fixture")

    os.environ.setdefault("CARGO_INCREMENTAL", "0")

    binary, runtime = build_fixture(
        arguments.mode == "listener", arguments.offline, arguments.release
    )

    subprocess.run(
        [
            arguments.node,
            "node_modules/typescript-compiler/bin/tsc",
            "-p",
            "tests/sdk/node/tsconfig.json",
        ],
        cwd=ROOT,
        check=True,
    )

    fixture = {
        "integrity": "tests::sdk_fixture::sdk_fixture_child",
        "phases": "tests::sdk_fixture::phase_fixture_child",
        "storage": "tests::sdk_fixture::storage_fixture_child",
        "listener": "listener_fixture_child",
    }[arguments.mode]

    python_cases = {
        "integrity": ["test_http_integrity.IntegrityTests", "test_http_raw.RawTests"],
        "phases": ["test_http_phases.PhaseTests"],
        "storage": ["test_http_storage.StorageTests"],
        "listener": ["test_http_listener.ListenerTests"],
    }[arguments.mode]

    with tempfile.TemporaryDirectory(prefix="mikrio-sdk-") as temporary:
        directory = Path(temporary).resolve()
        go_binary = directory / ("sdk-go.exe" if os.name == "nt" else "sdk-go")
        go_cases = []

        if arguments.go:
            run(
                [arguments.go, "test", "-mod=readonly", "./..."],
                cwd=ROOT / "tests/sdk/go",
            )

            run(
                [arguments.go, "build", "-mod=readonly", "-o", str(go_binary), "."],
                cwd=ROOT / "tests/sdk/go",
            )

            go_cases = json.loads(
                subprocess.check_output([str(go_binary), "--list"], encoding="utf-8")
            )

            if not go_cases or len(go_cases) != len(set(go_cases)):
                raise ValueError("missing or duplicate Go SDK cases")

        stage_runtime(runtime, directory)
        binary = Path(shutil.copy2(binary, directory / binary.name))
        env = environment()
        env["PYTHONPATH"] = str(ROOT / "tests/contract")
        records = directory / "results"
        records.mkdir()
        env["mikrio_sdk_live_record"] = str(records)
        env["mikrio_fixture_store"] = str(directory / "store")
        session = directory / "session"
        session.touch(mode=0o600)
        env["mikrio_fixture_session"] = str(session)

        with (directory / "fixture.log").open("w+", encoding="utf-8") as log:
            child = subprocess.Popen(
                [
                    str(binary),
                    "--ignored",
                    "--exact",
                    fixture,
                    "--nocapture",
                    "--format=terse",
                ],
                cwd=ROOT,
                env=env,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=log,
                encoding="utf-8",
            )

            messages: queue.Queue[str | None] = queue.Queue()

            def read_output() -> None:
                assert child.stdout is not None

                for line in child.stdout:
                    messages.put(line)

                messages.put(None)

            reader = threading.Thread(target=read_output, daemon=True)
            reader.start()

            try:
                while True:
                    line = messages.get(timeout=30)

                    if line is None:
                        raise RuntimeError("SDK fixture stopped before readiness")

                    if line.startswith("mikrio_ready "):
                        env["mikrio_fixture_endpoint"] = line.removeprefix(
                            "mikrio_ready "
                        ).strip()

                        break

                if arguments.mode == "listener":
                    peer, _, key, secret = session.read_text(
                        encoding="utf-8"
                    ).splitlines()

                    env.update(
                        mikrio_fixture_peer=peer,
                        mikrio_fixture_key=key,
                        mikrio_fixture_secret=secret,
                    )

                subprocess.run(
                    [sys.executable, "-m", "unittest", "-v", *python_cases],
                    cwd=ROOT / "tests/sdk",
                    env=env,
                    check=True,
                    timeout=90,
                )

                subprocess.run(
                    [
                        arguments.node,
                        "--test",
                        "--test-concurrency=1",
                        f"dist/http-{arguments.mode}.test.mjs",
                    ],
                    cwd=ROOT / "tests/sdk/node",
                    env=env,
                    check=True,
                    timeout=90,
                )

                if arguments.mode == "listener":
                    for suite in arguments.suite:
                        case = {
                            "multipart": "MultipartTests",
                            "post": "PostTests",
                            "transfers": "TransferTests",
                        }[suite]

                        subprocess.run(
                            [
                                sys.executable,
                                "-m",
                                "unittest",
                                "-v",
                                f"test_http_{suite}.{case}",
                            ],
                            cwd=ROOT / "tests/sdk",
                            env=env,
                            check=True,
                            timeout=300,
                        )

                        subprocess.run(
                            [
                                arguments.node,
                                "--test",
                                "--test-concurrency=1",
                                f"dist/http-{suite}.test.mjs",
                            ],
                            cwd=ROOT / "tests/sdk/node",
                            env=env,
                            check=True,
                            timeout=300,
                        )

                    for case in go_cases:
                        if case != "revoked_key_denied_on_both_listeners":
                            subprocess.run(
                                [str(go_binary), "--case", case],
                                cwd=directory,
                                env=env,
                                check=True,
                                timeout=60,
                            )

                    subprocess.run(
                        [
                            str(binary),
                            "--ignored",
                            "--exact",
                            "listener_revoke_child",
                            "--nocapture",
                        ],
                        cwd=ROOT,
                        env=env,
                        check=True,
                        timeout=30,
                    )

                    subprocess.run(
                        [
                            sys.executable,
                            "-m",
                            "unittest",
                            "-v",
                            "test_http_listener.RevokedTests",
                        ],
                        cwd=ROOT / "tests/sdk",
                        env=env,
                        check=True,
                        timeout=30,
                    )

                    subprocess.run(
                        [
                            arguments.node,
                            "--test",
                            "dist/http-listener-revoked.test.mjs",
                        ],
                        cwd=ROOT / "tests/sdk/node",
                        env=env,
                        check=True,
                        timeout=30,
                    )

                    if arguments.go:
                        subprocess.run(
                            [
                                str(go_binary),
                                "--case",
                                "revoked_key_denied_on_both_listeners",
                            ],
                            cwd=directory,
                            env=env,
                            check=True,
                            timeout=30,
                        )

                assert child.stdin is not None
                child.stdin.write("stop\n")
                child.stdin.flush()
                child.stdin.close()

                if child.wait(timeout=30) != 0:
                    raise RuntimeError(
                        "SDK fixture shutdown or independent native assertions failed"
                    )
            finally:
                if child.stdin is not None and not child.stdin.closed:
                    child.stdin.close()

                if child.poll() is None:
                    child.kill()
                    child.wait()

                reader.join(timeout=5)

                if child.stdout is not None:
                    child.stdout.close()

                if child.returncode:
                    log.seek(0)
                    print(log.read(), file=sys.stderr)


if __name__ == "__main__":
    main()
