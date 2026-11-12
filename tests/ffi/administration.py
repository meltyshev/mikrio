"""Exercise C administration through independent live C listeners."""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import http.client
import json
import os
import re
import runpy
import selectors
import socket
import subprocess
import time
import xml.etree.ElementTree as ET
from collections.abc import Callable
from contextlib import ExitStack
from ctypes import wintypes
from pathlib import Path
from types import TracebackType
from typing import Any, cast
from urllib.parse import urlsplit

SELECTOR = "administration_changes_reach_live_peer_listener"
BOOTSTRAP_SECRET = "synthetic-ffi-peer-secret"


def windows_pipe_readable(descriptor: int, timeout: float) -> bool:
    """Wait for bytes or EOF on an owned Windows pipe with one reader."""
    import msvcrt

    api = ctypes.WinDLL("kernel32", use_last_error=True)
    peek = api.PeekNamedPipe

    peek.argtypes = [
        wintypes.HANDLE,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_void_p,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.c_void_p,
    ]

    peek.restype = wintypes.BOOL
    handle = msvcrt.get_osfhandle(descriptor)
    deadline = time.monotonic() + timeout

    while True:
        available = wintypes.DWORD()

        if not peek(handle, None, 0, None, ctypes.byref(available), None):
            error = ctypes.get_last_error()

            if error in {109, 232}:  # Broken pipe / closed writer: read observes EOF.
                return True

            raise ctypes.WinError(error)

        if available.value:
            return True

        remaining = deadline - time.monotonic()

        if remaining <= 0:
            return False

        time.sleep(min(remaining, 0.01))


class Peer:
    """Own a bounded line protocol and reap its process on every exit."""

    def __init__(self, command: list[str], stderr: Path) -> None:
        self.buffer = bytearray()
        self.errors = stderr.open("xb")

        try:
            self.child = subprocess.Popen(
                command,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=self.errors,
                bufsize=0,
            )
        except BaseException:
            self.errors.close()
            raise

    def __enter__(self) -> Peer:
        return self

    def __exit__(
        self,
        kind: type[BaseException] | None,
        error: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        try:
            if self.child.poll() is None:
                self.child.terminate()

                try:
                    self.child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    self.child.kill()
                    self.child.wait(timeout=10)
        except BaseException as cleanup_error:
            if error is not None:
                raise BaseExceptionGroup(
                    "C peer flow and process cleanup failed", [error, cleanup_error]
                ) from None

            raise
        finally:
            if self.child.stdin is not None:
                self.child.stdin.close()

            if self.child.stdout is not None:
                self.child.stdout.close()

            self.errors.close()

    def line(self, timeout: float = 30) -> str:
        assert self.child.stdout is not None
        deadline = time.monotonic() + timeout

        with ExitStack() as stack:
            if os.name == "nt":

                def readable(timeout: float) -> bool:
                    assert self.child.stdout is not None
                    return windows_pipe_readable(self.child.stdout.fileno(), timeout)
            else:
                ready = stack.enter_context(selectors.DefaultSelector())
                ready.register(self.child.stdout, selectors.EVENT_READ)

                def readable(timeout: float) -> bool:
                    return bool(ready.select(timeout))

            while b"\n" not in self.buffer:
                remaining = deadline - time.monotonic()

                if remaining <= 0 or not readable(remaining):
                    raise TimeoutError("C peer did not acknowledge its next phase")

                chunk = os.read(self.child.stdout.fileno(), 1025)

                if not chunk:
                    raise RuntimeError("C peer exited before its next phase")

                self.buffer.extend(chunk)

                if len(self.buffer) > 1024:
                    raise ValueError("C peer exceeded the protocol bound")

        value, _, remaining_bytes = self.buffer.partition(b"\n")
        self.buffer[:] = remaining_bytes

        try:
            return value.removesuffix(b"\r").decode("ascii")
        except UnicodeDecodeError:
            raise ValueError("C peer emitted a non-ASCII protocol line") from None

    def expect(self, expected: str, timeout: float = 30) -> None:
        if self.line(timeout) != expected:
            # Never interpolate unexpected output: it may be a one-time secret.
            raise ValueError("C peer emitted an unexpected phase")

    def send(self, step: str) -> None:
        assert self.child.stdin is not None
        self.child.stdin.write((step + "\n").encode("ascii"))
        self.child.stdin.flush()

    def step(self, step: str) -> None:
        self.send(step)
        self.expect("DONE " + step)

    def stop(self) -> None:
        self.step("stop")

        if self.child.wait(timeout=30) != 0:
            raise RuntimeError("C peer failed during explicit shutdown")

        assert self.child.stdout is not None

        if self.buffer or self.child.stdout.read(1):
            raise ValueError("C peer emitted trailing protocol output")


def credential(peer: Peer) -> tuple[str, str]:
    value = peer.line()

    if re.fullmatch(r"KEY [A-Za-z0-9_-]{40} [A-Za-z0-9_-]{64}", value) is None:
        raise ValueError("C peer did not return one bounded credential")

    _, key, secret = value.split(" ")
    return key, secret


def endpoint(peer: Peer) -> tuple[str, int]:
    value = peer.line(timeout=120)

    if re.fullmatch(r"ENDPOINT http://127\.0\.0\.1:[0-9]+/?", value) is None:
        raise ValueError("C peer did not bind a loopback endpoint")

    parsed = urlsplit(value.removeprefix("ENDPOINT "))
    assert parsed.hostname == "127.0.0.1" and parsed.port is not None
    return parsed.hostname, parsed.port


def qualify(binary: Path, signer: Path, directory: Path) -> None:
    directory.mkdir()
    request = cast(Callable[..., bytes], runpy.run_path(str(signer))["request"])
    observations: list[dict[str, Any]] = []
    secrets = [BOOTSTRAP_SECRET]
    peers: list[Peer] = []
    started = time.monotonic()

    record: dict[str, Any] = {
        "status": "failing",
        "selector": SELECTOR,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "signer_sha256": hashlib.sha256(signer.read_bytes()).hexdigest(),
        "observations": observations,
        "limits": "Completed administration changes, ordinary loopback requests; "
        "no in-flight publication/revocation barrier or storage-interruption claim",
    }

    def exchange(
        target: tuple[str, int],
        identity: tuple[str, str],
        method: str,
        path: str,
        status: int,
        *,
        body: bytes = b"",
        expected_body: bytes | None = None,
        code: str | None = None,
    ) -> None:
        if time.monotonic() - started > 240:
            raise TimeoutError("C administration flow exceeded its watchdog")

        raw = request(
            method,
            path,
            "header",
            *identity,
            "us-east-1",
            "s3",
            body,
            {"host": f"{target[0]}:{target[1]}"},
        )

        with socket.create_connection(target, timeout=10) as connection:
            connection.sendall(raw)

            with http.client.HTTPResponse(connection) as response:
                response.begin()
                payload = response.read(4097)
                observed_status = response.status

        if len(payload) > 4096:
            raise ValueError("C listener exceeded the response body bound")

        observed_code = None

        if observed_status >= 400:
            observed_code = ET.fromstring(payload).findtext("Code")

        observations.append(
            {
                "method": method,
                "path": path,
                "status": observed_status,
                "code": observed_code,
                "body_bytes": len(payload),
                "body_sha256": hashlib.sha256(payload).hexdigest(),
            }
        )

        if observed_status != status or observed_code != code:
            raise AssertionError("C listener returned unexpected status or error code")

        if expected_body is not None and payload != expected_body:
            raise AssertionError("C listener returned different object bytes")

    try:
        with ExitStack() as stack:

            def launch(mode: str) -> Peer:
                peer = stack.enter_context(
                    Peer(
                        [str(binary), mode, str(directory / "store")],
                        directory / (mode + ".stderr"),
                    )
                )

                peers.append(peer)
                return peer

            listener = launch("listener-first")
            target = endpoint(listener)
            bootstrap = ("ffi-peer-first", BOOTSTRAP_SECRET)
            exchange(target, bootstrap, "PUT", "/c-peer", 200)
            exchange(target, bootstrap, "PUT", "/c-peer/key", 200, body=b"before")
            admin = launch("admin")
            first = credential(admin)
            secrets.append(first[1])
            admin.expect("DONE created")
            exchange(target, first, "GET", "/c-peer/key", 403, code="AccessDenied")
            admin.step("allow-get")
            exchange(target, first, "GET", "/c-peer/key", 200, expected_body=b"before")
            admin.step("allow-put")
            exchange(target, first, "GET", "/c-peer/key", 403, code="AccessDenied")
            exchange(target, first, "PUT", "/c-peer/key", 200, body=b"after")

            exchange(
                target, bootstrap, "GET", "/c-peer/key", 200, expected_body=b"after"
            )

            admin.step("delete-policy")

            exchange(
                target,
                first,
                "PUT",
                "/c-peer/key",
                403,
                body=b"denied",
                code="AccessDenied",
            )

            exchange(
                target, bootstrap, "GET", "/c-peer/key", 200, expected_body=b"after"
            )

            admin.step("disable")
            exchange(target, first, "GET", "/c-peer/key", 403, code="AccessDenied")
            admin.send("issue-disabled-key")
            second = credential(admin)
            secrets.append(second[1])
            admin.expect("DONE issue-disabled-key")
            exchange(target, second, "GET", "/c-peer/key", 403, code="AccessDenied")
            admin.step("enable")

            for identity in (first, second):
                exchange(
                    target, identity, "GET", "/c-peer/key", 200, expected_body=b"after"
                )

            admin.step("revoke-first")
            exchange(target, first, "GET", "/c-peer/key", 403, code="AccessDenied")
            exchange(target, second, "GET", "/c-peer/key", 200, expected_body=b"after")
            admin.step("disable-enable")
            exchange(target, first, "GET", "/c-peer/key", 403, code="AccessDenied")
            exchange(target, second, "GET", "/c-peer/key", 200, expected_body=b"after")
            listener.stop()
            listener = launch("listener-second")
            target = endpoint(listener)

            exchange(
                target, bootstrap, "GET", "/c-peer/key", 403, code="InvalidAccessKeyId"
            )

            bootstrap = ("ffi-peer-second", BOOTSTRAP_SECRET)

            exchange(
                target, bootstrap, "GET", "/c-peer/key", 200, expected_body=b"after"
            )

            exchange(target, second, "GET", "/c-peer/key", 200, expected_body=b"after")
            admin.step("delete")
            exchange(target, second, "GET", "/c-peer/key", 403, code="AccessDenied")

            exchange(
                target, bootstrap, "GET", "/c-peer/key", 200, expected_body=b"after"
            )

            admin.send("recreate")
            third = credential(admin)
            secrets.append(third[1])
            admin.expect("DONE recreate")
            exchange(target, third, "GET", "/c-peer/key", 403, code="AccessDenied")
            exchange(target, second, "GET", "/c-peer/key", 403, code="AccessDenied")
            admin.stop()

            exchange(
                target, bootstrap, "GET", "/c-peer/key", 200, expected_body=b"after"
            )

            listener.stop()

        if len({peer.child.pid for peer in peers}) != 3:
            raise AssertionError(
                "C listener and administration did not use distinct processes"
            )

        if any(
            secret.encode() in path.read_bytes()
            for path in directory.glob("*.stderr")
            for secret in secrets
        ):
            raise AssertionError("C peer emitted a signing secret to diagnostics")

        record["status"] = "passing"
    finally:
        record["elapsed_seconds"] = time.monotonic() - started

        record["processes"] = [
            {"pid": peer.child.pid, "exit_code": peer.child.returncode}
            for peer in peers
        ]

        (directory / "result.json").write_text(json.dumps(record, indent=2) + "\n")

    print("PASS " + SELECTOR, flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("signer", type=Path)
    parser.add_argument("directory", type=Path)
    arguments = parser.parse_args()
    qualify(arguments.binary, arguments.signer, arguments.directory)
