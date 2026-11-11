"""Acknowledged ordinary writes survive abrupt writer-process termination."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import queue
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

from mikrio import Store, Tag

PAYLOAD = bytes(range(251)) * 263
ETAG = '"' + hashlib.md5(PAYLOAD, usedforsecurity=False).hexdigest() + '"'
SHA256 = base64.b64encode(hashlib.sha256(PAYLOAD).digest()).decode()


def writer(directory: str, replacement: bool) -> None:
    with Store.open(directory=directory) as store:
        store.create_bucket(bucket="recovery")

        if replacement:
            store.put_object(bucket="recovery", key="acknowledged", body=b"predecessor")

        output = store.put_object(
            bucket="recovery",
            key="acknowledged",
            body=PAYLOAD,
            content_length=len(PAYLOAD),
            metadata={"generation": "acknowledged"},
            tags=[Tag(key="state", value="committed")],
            checksum_algorithm="SHA256",
            checksum_sha256=SHA256,
        )

        assert output.etag == ETAG and output.checksum_sha256 == SHA256

        print(
            json.dumps({"store_id": store.info.store_id, "etag": output.etag}),
            flush=True,
        )

        # Only the parent terminates this process after observing actual success.
        # Reaching EOF is a fixture failure, never the intended shutdown path.
        sys.stdin.buffer.read(1)
        raise AssertionError("writer was released instead of killed")


def verify(directory: str, store_id: str) -> None:
    for _ in range(2):
        with Store.open(directory=directory) as store:
            assert store.info.store_id == store_id

            output = store.get_object(
                bucket="recovery", key="acknowledged", checksum_mode="ENABLED"
            )

            with output.body as body:
                actual = body.read_all(max_bytes=len(PAYLOAD))

            assert actual == PAYLOAD
            assert base64.b64encode(hashlib.sha256(actual).digest()).decode() == SHA256
            assert output.etag == ETAG and output.checksum_sha256 == SHA256
            assert output.content_length == len(PAYLOAD)
            assert output.metadata == {"generation": "acknowledged"}
            tags = store.get_object_tagging(bucket="recovery", key="acknowledged").tags
            assert [(tag.key, tag.value) for tag in tags] == [("state", "committed")]

    print("PASS independent_reopen_readback")


class ProcessRecoveryTests(unittest.TestCase):
    def check_writer_death(self, *, replacement: bool) -> None:
        root = Path(tempfile.mkdtemp(prefix="mikrio-process-recovery-"))

        try:
            self.check_recovery(root, replacement=replacement)
        except BaseException as error:
            error.add_note(f"Recovery fixture retained at {root}")
            raise

        shutil.rmtree(root)

    def check_recovery(self, root: Path, *, replacement: bool) -> None:
        directory = root / "store"
        script = str(Path(__file__).resolve())
        reader: threading.Thread | None = None

        with (root / "writer.stderr").open("wb") as errors:
            child = subprocess.Popen(
                [
                    sys.executable,
                    "-I",
                    script,
                    "replace" if replacement else "write",
                    str(directory),
                ],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=errors,
            )

        try:
            assert child.stdout is not None and child.stdin is not None
            observations: queue.Queue[bytes] = queue.Queue(maxsize=1)
            output = child.stdout

            reader = threading.Thread(
                target=lambda: observations.put(output.readline(4097))
            )

            reader.start()
            line = observations.get(timeout=30)
            self.assertLessEqual(len(line), 4096)
            self.assertTrue(line.endswith(b"\n"), "writer did not acknowledge the PUT")
            acknowledgment = json.loads(line)
            self.assertEqual(acknowledgment["etag"], ETAG)
            self.assertIsNone(child.poll(), "writer exited before the kill")
            child.kill()
            status = child.wait(timeout=10)
            self.assertEqual(status, 1 if os.name == "nt" else -signal.SIGKILL)
            self.assertEqual(output.read(), b"")
        finally:
            if child.poll() is None:
                child.kill()

            child.wait(timeout=10)

            if child.stdin is not None:
                child.stdin.close()

            if reader is not None and reader.ident is not None:
                reader.join(timeout=10)

                self.assertFalse(
                    reader.is_alive(), "writer observation thread did not settle"
                )

            if child.stdout is not None:
                child.stdout.close()

        stdout_path = root / "verify.stdout"
        stderr_path = root / "verify.stderr"

        with (
            stdout_path.open("wb") as verifier_output,
            stderr_path.open("wb") as verifier_errors,
        ):
            result = subprocess.run(
                [
                    sys.executable,
                    "-I",
                    script,
                    "verify",
                    str(directory),
                    acknowledgment["store_id"],
                ],
                stdout=verifier_output,
                stderr=verifier_errors,
                timeout=30,
                check=False,
            )

        self.assertEqual(result.returncode, 0, stderr_path.read_text())
        self.assertEqual(stdout_path.read_text(), "PASS independent_reopen_readback\n")

    def test_acknowledged_put_survives_kill_and_independent_reopen(self) -> None:
        self.check_writer_death(replacement=False)

    def test_acknowledged_replacement_survives_kill_and_independent_reopen(
        self,
    ) -> None:
        self.check_writer_death(replacement=True)


if __name__ == "__main__":
    mode, directory, *remaining = sys.argv[1:]

    if mode == "verify":
        verify(directory, remaining[0])
    else:
        assert mode in {"write", "replace"}
        writer(directory, mode == "replace")
