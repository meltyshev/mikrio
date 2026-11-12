"""Real subprocess controls for bounded, secret-free C peer observations."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

from administration import Peer, credential


class AdministrationProtocolTests(unittest.TestCase):
    def test_crlf_and_buffered_lines_preserve_each_phase(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with Peer(
                [
                    sys.executable,
                    "-c",
                    "import sys; sys.stdout.buffer.write(b'FIRST\\r\\nSECOND\\n')",
                ],
                Path(temporary) / "stderr",
            ) as peer:
                peer.expect("FIRST")
                peer.expect("SECOND")

            self.assertIsNotNone(peer.child.returncode)

    def test_stop_acknowledgement_and_eof_complete_clean_shutdown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with Peer(
                [
                    sys.executable,
                    "-c",
                    "print('READY', flush=True); "
                    "assert input() == 'stop'; print('DONE stop', flush=True)",
                ],
                Path(temporary) / "stderr",
            ) as peer:
                peer.expect("READY")
                peer.stop()

            self.assertEqual(peer.child.returncode, 0)

    def test_premature_exit_cannot_acknowledge_a_change(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with Peer(
                [sys.executable, "-c", "pass"], Path(temporary) / "stderr"
            ) as peer:
                with self.assertRaisesRegex(RuntimeError, "exited before"):
                    peer.expect("DONE committed")

            self.assertIsNotNone(peer.child.returncode)

    def test_unexpected_secret_is_not_copied_into_failure(self) -> None:
        secret = "synthetic-one-time-secret"

        with tempfile.TemporaryDirectory() as temporary:
            with Peer(
                [sys.executable, "-c", f"print({secret!r})"],
                Path(temporary) / "stderr",
            ) as peer:
                with self.assertRaisesRegex(ValueError, "unexpected phase") as failed:
                    peer.expect("DONE committed")

            self.assertNotIn(secret, str(failed.exception))
            self.assertIsNotNone(peer.child.returncode)

    def test_oversized_output_is_refused_and_child_is_reaped(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with Peer(
                [sys.executable, "-c", "print('x' * 1025, flush=True); input()"],
                Path(temporary) / "stderr",
            ) as peer:
                with self.assertRaisesRegex(ValueError, "protocol bound"):
                    peer.line()

            self.assertIsNotNone(peer.child.returncode)

    def test_stalled_peer_is_bounded_after_an_observed_ready_phase(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with Peer(
                [sys.executable, "-c", "print('READY', flush=True); input()"],
                Path(temporary) / "stderr",
            ) as peer:
                peer.expect("READY")

                with self.assertRaisesRegex(TimeoutError, "did not acknowledge"):
                    peer.line(timeout=0.05)

            self.assertIsNotNone(peer.child.returncode)

    def test_non_ascii_output_is_rejected_without_disclosure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with Peer(
                [
                    sys.executable,
                    "-c",
                    "import sys; sys.stdout.buffer.write(b'\\xff\\n')",
                ],
                Path(temporary) / "stderr",
            ) as peer:
                with self.assertRaisesRegex(
                    ValueError, "^C peer emitted a non-ASCII protocol line$"
                ):
                    peer.line()

            self.assertIsNotNone(peer.child.returncode)

    def test_wrong_credential_shape_is_rejected_without_disclosure(self) -> None:
        secret = "synthetic-malformed-secret"

        with tempfile.TemporaryDirectory() as temporary:
            with Peer(
                [sys.executable, "-c", f"print('KEY invalid {secret}')"],
                Path(temporary) / "stderr",
            ) as peer:
                with self.assertRaisesRegex(ValueError, "bounded credential") as failed:
                    credential(peer)

            self.assertNotIn(secret, str(failed.exception))
            self.assertIsNotNone(peer.child.returncode)

    def test_trailing_output_does_not_pass_clean_shutdown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with Peer(
                [sys.executable, "-c", "input(); print('DONE stop'); print('extra')"],
                Path(temporary) / "stderr",
            ) as peer:
                with self.assertRaisesRegex(ValueError, "trailing protocol"):
                    peer.stop()

            self.assertEqual(peer.child.returncode, 0)


if __name__ == "__main__":
    unittest.main()
