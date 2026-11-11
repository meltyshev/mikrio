"""Owned listener behavior through the ordinary installed Python extension."""

from __future__ import annotations

import concurrent.futures
import gc
import http.client
import os
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from urllib.parse import urlsplit

from mikrio import Credentials, Server, StorageError, Store


def get_listener_status(server: Server) -> int:
    address = urlsplit(server.endpoint)
    assert address.hostname is not None
    connection = http.client.HTTPConnection(address.hostname, address.port, timeout=5)

    try:
        connection.request("HEAD", "/listener")
        response = connection.getresponse()
        response.read()
        return response.status
    finally:
        connection.close()


class ListenerTests(unittest.TestCase):
    @unittest.skipUnless(hasattr(os, "fork"), "requires Unix fork semantics")
    def test_context_cleanup_retains_primary_history_without_exception_setattr(
        self,
    ) -> None:
        # An owned fork gives every native close a deterministic pre-runtime
        # failure. The parent retains and subsequently uses the real resources.
        script = """
import os
import sys
import traceback

from mikrio import Store


class FrozenInterrupt(KeyboardInterrupt):
    def __setattr__(self, name: str, value: object) -> None:
        raise AssertionError("exception attribute hook must not run")


with Store.open(directory=sys.argv[1]) as store:
    store.create_bucket(bucket="context-errors")
    store.put_object(bucket="context-errors", key="selected", body=b"parent bytes")

    with store.listen() as server:
        with store.get_object(bucket="context-errors", key="selected").body as body:
            child = os.fork()

            if child == 0:
                try:
                    for handle in (server, body, store):
                        for primary in (
                            FrozenInterrupt("primary"),
                            ValueError("primary"),
                        ):
                            previous = LookupError("previous context")
                            cause = OSError("original cause")
                            BaseException.__setattr__(primary, "__context__", previous)
                            BaseException.__setattr__(primary, "__cause__", cause)

                            assert (
                                handle.__exit__(type(primary), primary, None) is False
                            )

                            cleanup = primary.__context__
                            assert isinstance(cleanup, RuntimeError), cleanup
                            assert "inherited after fork" in str(cleanup)
                            assert cleanup.__context__ is previous
                            assert primary.__cause__ is cause

                    os._exit(0)
                except BaseException:
                    traceback.print_exc()
                    os._exit(1)

            _, status = os.waitpid(child, 0)
            assert os.waitstatus_to_exitcode(status) == 0
            assert body.read_all(max_bytes=100) == b"parent bytes"

        store.head_object(bucket="context-errors", key="selected")
"""

        with tempfile.TemporaryDirectory(prefix="mikrio-context-errors-") as root:
            path = Path(root) / "store"
            Store.open(directory=path).close()

            subprocess.run(
                [sys.executable, "-I", "-c", script, str(path)],
                check=True,
                timeout=20,
            )

    def test_ready_close_relisten_parent_close_and_detached_credentials(self) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-listener-") as root:
            path = Path(root) / "store"

            with (
                Store.open(directory=path) as store,
                Store.open(directory=path) as peer,
            ):
                store.create_bucket(bucket="listener")
                first = store.listen()
                self.addCleanup(first.close)

                with peer.listen() as second:
                    self.assertEqual(get_listener_status(first), 403)
                    self.assertEqual(get_listener_status(second), 403)
                    snapshot = first.credentials

                    self.assertNotEqual(
                        snapshot.access_key_id, second.credentials.access_key_id
                    )

                    self.assertNotIn(snapshot.secret_access_key, repr(snapshot))

                    with self.assertRaises(AttributeError):
                        snapshot.secret_access_key = "changed"  # type: ignore[misc]

                    with self.assertRaises(StorageError) as occupied:
                        store.listen()

                    self.assertEqual(occupied.exception.code, "ListenerAlreadyRunning")

                    with concurrent.futures.ThreadPoolExecutor(2) as executor:
                        futures = [executor.submit(first.close) for _ in range(2)]

                        for future in futures:
                            future.result(timeout=10)

                    self.assertEqual(
                        first.credentials.secret_access_key, snapshot.secret_access_key
                    )

                    store.head_bucket(bucket="listener")

                    with store.listen(credentials=snapshot) as replacement:
                        self.assertEqual(get_listener_status(replacement), 403)
                        store.close()
                        self.assertEqual(replacement.region, "us-east-1")
                        address = urlsplit(replacement.endpoint)
                        assert address.hostname is not None and address.port is not None

                        with self.assertRaises(ConnectionRefusedError):
                            with socket.create_connection(
                                (address.hostname, address.port), timeout=5
                            ):
                                self.fail("closed listener accepted a connection")

                    self.assertEqual(get_listener_status(second), 403)

    def test_invalid_bind_options_and_parent_collection_preserve_independent_store(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-listener-values-") as root:
            path = Path(root) / "store"
            # Parent collection is the scenario; registering close would retain it.
            store = Store.open(directory=path)

            try:
                peer = Store.open(directory=path)
            except BaseException:
                store.__exit__(*sys.exc_info())
                raise

            with peer:
                try:
                    for host in ("localhost", "", "127.0.0.1:9000"):
                        with self.subTest(host=host):
                            with self.assertRaises(StorageError) as invalid:
                                store.listen(host=host)

                            self.assertEqual(invalid.exception.code, "InvalidArgument")

                    for port in (-1, 65536, 2**64):
                        with self.subTest(port=port):
                            with self.assertRaises(StorageError) as invalid:
                                store.listen(port=port)

                            self.assertEqual(invalid.exception.code, "InvalidArgument")

                    with self.assertRaises(TypeError):
                        store.listen(port=True)

                    with self.assertRaises(StorageError) as invalid:
                        store.listen(public_endpoint="")

                    self.assertEqual(invalid.exception.code, "InvalidArgument")

                    with self.assertRaises(StorageError) as invalid:
                        store.listen(
                            credentials=Credentials(
                                access_key_id="mikrio_aReserved",
                                secret_access_key="secret",
                            )
                        )

                    self.assertEqual(invalid.exception.code, "InvalidArgument")

                    server = store.listen(
                        host="0.0.0.0", public_endpoint="https://example.invalid/"
                    )
                except BaseException:
                    store.__exit__(*sys.exc_info())
                    raise

                with server:
                    endpoint = server.endpoint
                    del store
                    gc.collect()
                    self.assertTrue(endpoint.startswith("http://127.0.0.1:"))

                peer.create_bucket(bucket="after-parent-drop")
