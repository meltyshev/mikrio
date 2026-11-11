"""Run against an installed wheel from outside the source checkout."""

from __future__ import annotations

import gc
import os
import pickle
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

import mikrio
import native_guard
from mikrio import StorageError, Store
from native_guard import lock, unlock


class LifecycleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="mikrio-python-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def open_store(self, path: Path | None = None) -> Store:
        store = Store.open(directory=path or self.root / "store")
        self.addCleanup(store.close)
        return store

    def test_installed_module_and_immutable_identity_survive_reopen(self) -> None:
        module = Path(mikrio.__file__).resolve()
        self.assertIn("site-packages", module.parts)
        self.assertTrue(module.with_name("py.typed").is_file())
        self.assertTrue(module.with_name("__init__.pyi").is_file())
        store = self.open_store()
        snapshot = store.info
        self.assertRegex(snapshot.store_id, "^[0-9a-f]{32}$")
        self.assertEqual(snapshot.format_version, 1)
        self.assertEqual(snapshot.region, "us-east-1")
        # Rust retains Windows' extended-length canonical spelling. Compare
        # the actual directory identity without assuming a host path prefix.
        self.assertTrue(Path(snapshot.directory).is_absolute())
        self.assertTrue(Path(snapshot.directory).samefile(self.root / "store"))

        with self.assertRaises(AttributeError):
            snapshot.region = "eu-west-1"  # type: ignore[misc]

        store.close()
        store.close()
        reopened = self.open_store()
        self.assertEqual(reopened.info.store_id, snapshot.store_id)
        self.assertEqual(store.info.store_id, snapshot.store_id)

    def test_independent_close_preserves_native_guard(self) -> None:
        first = self.open_store()
        second = self.open_store()
        first.close()

        with (self.root / "store" / ".mikrio.lock").open("r+b") as guard:
            with self.assertRaises(BlockingIOError):
                lock(guard)

            second.close()
            lock(guard)
            unlock(guard)

    def test_existing_only_refuses_uninitialized_paths_and_reopens_identity(
        self,
    ) -> None:
        missing = self.root / "missing" / "store"
        empty = self.root / "empty"
        empty.mkdir()

        for path in (missing, empty):
            with self.assertRaises(StorageError) as caught:
                Store.open(directory=path, create_if_missing=False)

            self.assertEqual(caught.exception.code, "InvalidStoreDirectory")

        self.assertFalse(missing.parent.exists())
        self.assertEqual(list(empty.iterdir()), [])

        with Store.open(directory=empty) as first:
            with Store.open(directory=empty, create_if_missing=False) as peer:
                self.assertEqual(peer.info.store_id, first.info.store_id)

        with Store.open(directory=empty, create_if_missing=False) as reopened:
            self.assertEqual(reopened.info.store_id, first.info.store_id)

        for invalid in (None, 0, 1, "false"):
            with self.assertRaises(TypeError):
                Store.open(directory=missing, create_if_missing=invalid)  # type: ignore[arg-type]

        self.assertFalse(missing.parent.exists())

    def test_domain_errors_are_typed_and_preserve_unrelated_files(self) -> None:
        unrelated = self.root / "unrelated"
        unrelated.mkdir()
        payload_path = unrelated / "keep"
        payload_path.write_bytes(b"unrelated bytes")

        with self.assertRaises(StorageError) as caught:
            Store.open(directory=unrelated)

        self.assertEqual(caught.exception.code, "InvalidStoreDirectory")
        self.assertIsNone(caught.exception.mutation_outcome)
        self.assertIsNone(caught.exception.partial_result)
        self.assertEqual(payload_path.read_bytes(), b"unrelated bytes")
        self.assertEqual(list(unrelated.iterdir()), [payload_path])

        with self.assertRaises(StorageError) as invalid:
            Store.open(directory=self.root / "invalid", region="UPPER")

        self.assertEqual(invalid.exception.code, "InvalidArgument")
        self.assertFalse((self.root / "invalid").exists())

    def test_persisted_region_and_conflict(self) -> None:
        with Store.open(directory=self.root / "store", region="eu-west-1") as store:
            identity = store.info.store_id

        reopened = self.open_store()
        self.assertEqual(reopened.info.region, "eu-west-1")
        self.assertEqual(reopened.info.store_id, identity)

        with self.assertRaises(StorageError) as mismatch:
            Store.open(directory=self.root / "store", region="us-east-1")

        self.assertEqual(mismatch.exception.code, "RegionMismatch")

    def test_context_manager_preserves_control_flow_exception(self) -> None:
        interruption = KeyboardInterrupt("controlled interruption")

        with self.assertRaises(KeyboardInterrupt) as caught:
            with self.open_store() as store:
                self.assertEqual(store.info.region, "us-east-1")
                raise interruption

        self.assertIs(caught.exception, interruption)
        self.assertEqual(self.open_store().info.store_id, store.info.store_id)

    def test_binding_misuse_and_nonportable_paths_are_rejected(self) -> None:
        with self.assertRaises(TypeError):
            Store.open(directory=b"bytes")  # type: ignore[arg-type]

        with self.assertRaises(TypeError):
            Store.open(self.root / "positional")  # type: ignore[call-arg]

        with self.assertRaises(TypeError):
            Store.open(directory=self.root, unexpected=True)  # type: ignore[call-arg]

        with self.assertRaises(StorageError) as invalid:
            Store.open(directory="\ud800")

        self.assertEqual(invalid.exception.code, "InvalidArgument")

        with self.assertRaises(StorageError) as region:
            Store.open(directory=self.root / "invalid-region", region="\ud800")

        self.assertEqual(region.exception.code, "InvalidArgument")
        self.assertEqual(list(self.root.iterdir()), [])

    def test_newer_format_is_refused_without_resetting_identity(self) -> None:
        store = self.open_store()
        identity = store.info.store_id
        store.close()
        database_path = self.root / "store" / "metadata.sqlite3"

        with closing(sqlite3.connect(database_path)) as connection, connection:
            connection.execute("UPDATE store_metadata SET format_version = 99")

        with self.assertRaises(StorageError) as caught:
            Store.open(directory=self.root / "store")

        self.assertEqual(caught.exception.code, "UnsupportedStoreVersion")

        with closing(sqlite3.connect(database_path)) as connection, connection:
            actual = connection.execute(
                "SELECT store_id, format_version FROM store_metadata"
            ).fetchone()

        self.assertEqual(actual, (bytes.fromhex(identity), 99))

    def test_handles_cannot_be_pickled(self) -> None:
        with self.assertRaises(TypeError):
            pickle.dumps(self.open_store())

    def test_io_error_preserves_trusted_native_cause(self) -> None:
        parent = self.root / "file"
        parent.write_text("keep")

        with self.assertRaises(StorageError) as caught:
            Store.open(directory=parent / "child")

        self.assertEqual(caught.exception.code, "IoError")
        cause: BaseException | None = caught.exception

        while cause is not None and not isinstance(cause, OSError):
            cause = cause.__cause__

        self.assertIsInstance(cause, OSError)
        self.assertEqual(parent.read_text(), "keep")

    def test_blocked_open_releases_gil_and_preserves_keyboard_interrupt(self) -> None:
        # A real external guard holds native open pending. The Python thread can
        # only make progress and schedule SIGINT if the native waiter detaches.
        # Prepare durable layout before the child lifetime watchdog; the child
        # still owns its native runtime, guard, interruption and independent reopen.
        Store.open(directory=self.root / "store").close()

        script = """
import _thread
import runpy
import sys
import threading
import time
from pathlib import Path

from mikrio import Store

guards = runpy.run_path(sys.argv[2])
lock = guards["lock"]
unlock = guards["unlock"]
directory = Path(sys.argv[1])

with Store.open(directory=directory) as store:
    identity = store.info.store_id

with (directory / ".mikrio.lock").open("r+b") as guard:
    lock(guard)
    started = threading.Event()
    progressed = threading.Event()

    def interrupt() -> None:
        assert started.wait(2)
        time.sleep(0.2)
        progressed.set()
        _thread.interrupt_main()

    thread = threading.Thread(target=interrupt)
    thread.start()
    start = time.monotonic()
    started.set()

    try:
        unexpected = Store.open(directory=directory)
    except KeyboardInterrupt:
        assert progressed.is_set()
        assert time.monotonic() - start < 2
    else:
        unexpected.close()
        raise AssertionError("blocked open unexpectedly succeeded")
    finally:
        thread.join(2)
        assert not thread.is_alive()
        unlock(guard)

with Store.open(directory=directory) as reopened:
    assert reopened.info.store_id == identity
"""

        subprocess.run(
            [
                sys.executable,
                "-I",
                "-c",
                script,
                str(self.root / "store"),
                native_guard.__file__,
            ],
            check=True,
            timeout=15,
        )

    def test_import_is_offline_and_does_not_initialize_store(self) -> None:
        script = """
import os
import sys


def audit(event: str, args: tuple[object, ...]) -> None:
    if event in {"socket.__new__", "subprocess.Popen", "os.fork", "os.mkdir"}:
        raise AssertionError(event)


sys.addaudithook(audit)
import mikrio  # noqa: E402 - audit module initialization itself.

assert os.listdir(".") == []
assert set(mikrio.__all__) == {
    "BinaryReader",
    "CreateBucketOutput",
    "DeleteBucketOutput",
    "DeleteObjectOutput",
    "GetBucketLocationOutput",
    "GetObjectOutput",
    "HeadBucketOutput",
    "HeadObjectOutput",
    "ObjectBody",
    "PutObjectOutput",
    "StorageError",
    "Store",
    "StoreInfo",
    "Tag",
    "Credentials",
    "Server",
    "OwnerInfo",
    "PrincipalInfo",
    "AccessKeyInfo",
    "CreateAccessKeyOutput",
    "ListPrincipalsOutput",
    "ListAccessKeysOutput",
    "GetPrincipalPolicyOutput",
    "PutPrincipalPolicyOutput",
    "DeletePrincipalPolicyOutput",
    "DeleteAccessKeyOutput",
    "DeletePrincipalOutput",
    "PublicAccessBlockConfiguration",
    "AclOwner",
    "AclGrant",
    "PutBucketPolicyOutput",
    "GetBucketPolicyOutput",
    "DeleteBucketPolicyOutput",
    "GetBucketPolicyStatusOutput",
    "PutPublicAccessBlockOutput",
    "GetPublicAccessBlockOutput",
    "DeletePublicAccessBlockOutput",
    "PutBucketOwnershipControlsOutput",
    "GetBucketOwnershipControlsOutput",
    "GetBucketAclOutput",
    "GetObjectAclOutput",
    "CorsRule",
    "PutBucketCorsOutput",
    "GetBucketCorsOutput",
    "DeleteBucketCorsOutput",
    "LifecycleFilter",
    "LifecycleExpiration",
    "AbortIncompleteMultipartUpload",
    "LifecycleRule",
    "PutBucketLifecycleConfigurationOutput",
    "GetBucketLifecycleConfigurationOutput",
    "DeleteBucketLifecycleOutput",
    "GetObjectTaggingOutput",
    "PutObjectTaggingOutput",
    "DeleteObjectTaggingOutput",
    "CommonPrefix",
    "ObjectSummary",
    "ListObjectsV2Output",
    "BucketSummary",
    "ListBucketsOutput",
    "ObjectIdentifier",
    "DeletedObject",
    "DeleteError",
    "DeleteObjectError",
    "DeleteObjectsOutput",
    "DeleteObjectProgress",
    "DeleteObjectsProgress",
    "ObjectRef",
    "CompletedPart",
    "CopyObjectOutput",
    "CreateMultipartUploadOutput",
    "UploadPartOutput",
    "UploadPartCopyOutput",
    "CompleteMultipartUploadOutput",
    "AbortMultipartUploadOutput",
    "PartSummary",
    "MultipartUploadSummary",
    "ListPartsOutput",
    "ListMultipartUploadsOutput",
    "Checksum",
    "ObjectPart",
    "ObjectParts",
    "GetObjectAttributesOutput",
}
"""

        subprocess.run(
            [sys.executable, "-I", "-B", "-c", script],
            cwd=self.root,
            check=True,
            timeout=10,
        )

    def test_saturated_acquisition_cannot_prevent_close(self) -> None:
        # First rejection proves all 32 admission slots are occupied while an
        # OS guard keeps acquisition pending. Shutdown must not need a slot.
        Store.open(directory=self.root / "live").close()
        Store.open(directory=self.root / "blocked").close()

        script = """
import queue
import runpy
import sys
import threading
from pathlib import Path

from mikrio import StorageError, Store

guards = runpy.run_path(sys.argv[2])
lock = guards["lock"]
unlock = guards["unlock"]
root = Path(sys.argv[1])

with Store.open(directory=root / "live") as live:
    with Store.open(directory=root / "blocked") as blocked:
        identity = blocked.info.store_id

    start = threading.Barrier(41)
    completed = queue.Queue()

    def acquire() -> None:
        try:
            start.wait(5)

            with Store.open(directory=root / "blocked") as store:
                assert store.info.store_id == identity

            completed.put(None)
        except BaseException as error:
            completed.put(error)

    with (root / "blocked" / ".mikrio.lock").open("r+b") as guard:
        lock(guard)
        threads = [threading.Thread(target=acquire) for _ in range(40)]

        try:
            for thread in threads:
                thread.start()

            start.wait(5)
            first = completed.get(timeout=2)
            assert isinstance(first, StorageError) and first.code == "StoreBusy", first
            live.close()
        finally:
            unlock(guard)

            for thread in threads:
                thread.join(10)
                assert not thread.is_alive()

            live.close()

    for _ in range(39):
        result = completed.get_nowait()

        assert result is None or (
            isinstance(result, StorageError) and result.code == "StoreBusy"
        ), result

    with Store.open(directory=root / "blocked") as reopened:
        assert reopened.info.store_id == identity
"""

        subprocess.run(
            [sys.executable, "-I", "-c", script, str(self.root), native_guard.__file__],
            check=True,
            timeout=25,
        )

    def test_spawned_interpreter_reopens_same_identity(self) -> None:
        store = self.open_store()

        script = """
import sys

from mikrio import Store

with Store.open(directory=sys.argv[1]) as store:
    print(store.info.store_id)
"""

        child = subprocess.run(
            [sys.executable, "-I", "-c", script, str(self.root / "store")],
            check=True,
            capture_output=True,
            text=True,
            timeout=15,
        )

        self.assertEqual(child.stdout.strip(), store.info.store_id)

    @unittest.skipUnless(hasattr(os, "fork"), "requires Unix fork semantics")
    def test_inherited_handles_and_fresh_open_are_refused_after_fork(self) -> None:
        # Isolate process-global fork behavior in an owned subprocess. The child
        # explicitly collects inherited wrappers, exercising their finalizers.
        Store.open(directory=self.root / "store").close()

        script = """
import gc
import os
import sys

from mikrio import Store

store = Store.open(directory=sys.argv[1])
identity = store.info.store_id
child = os.fork()

if child == 0:
    try:
        for use in (
            lambda: store.info,
            store.close,
            lambda: Store.open(directory=sys.argv[1]),
        ):
            try:
                use()
            except RuntimeError as error:
                assert "inherited after fork" in str(error)
            else:
                os._exit(2)

        del use, store
        gc.collect()
    except BaseException:
        os._exit(3)

    os._exit(0)

try:
    _, status = os.waitpid(child, 0)
    assert os.waitstatus_to_exitcode(status) == 0
    assert store.info.store_id == identity
finally:
    store.close()

with Store.open(directory=sys.argv[1]) as reopened:
    assert reopened.info.store_id == identity
"""

        subprocess.run(
            [sys.executable, "-I", "-c", script, str(self.root / "store")],
            check=True,
            timeout=20,
        )

    def tearDown(self) -> None:
        gc.collect()


if __name__ == "__main__":
    unittest.main()
