"""Administration through the installed package, including independent processes."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from mikrio import StorageError, Store

POLICY = (
    '{ "Version":"2012-10-17",'
    ' "Statement":{"Effect":"Allow",'
    '"Action":"s3:GetObject",'
    '"Resource":"arn:aws:s3:::photos/*"}}'
)


class AdministrationTests(unittest.TestCase):
    def test_rotation_policy_and_explicit_deletion_survive_reopen(self) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-admin-") as temporary:
            path = Path(temporary) / "store"

            with Store.open(directory=path) as store:
                owner = store.get_owner_info()
                principal = store.create_principal(name="app")
                self.assertTrue(principal.is_enabled)

                store.put_principal_policy(
                    principal_id=principal.principal_id, document=POLICY
                )

                policy = store.get_principal_policy(principal_id=principal.principal_id)
                self.assertEqual(policy.document, POLICY)
                self.assertNotIn(POLICY, repr(policy))
                key = store.create_access_key(principal_id=principal.principal_id)
                second = store.create_access_key(principal_id=principal.principal_id)

                self.assertNotEqual(
                    key.access_key.access_key_id, second.access_key.access_key_id
                )

                self.assertNotIn(key.secret_access_key, repr(key))

                with self.assertRaises(StorageError) as denied:
                    store.delete_principal(principal_id=principal.principal_id)

                self.assertEqual(denied.exception.code, "PrincipalHasAccessKeys")
                self.assertEqual(denied.exception.mutation_outcome, "NotCommitted")

                disabled = store.set_principal_enabled(
                    principal_id=principal.principal_id, is_enabled=False
                )

                self.assertFalse(disabled.is_enabled)
                self.assertEqual(disabled.created_at, principal.created_at)

            self.assertEqual(len(owner.account_id), 12)
            self.assertEqual(len(owner.canonical_id), 64)
            self.assertEqual(len(key.secret_access_key), 64)

            with Store.open(directory=path) as reopened:
                self.assertEqual(reopened.get_owner_info().account_id, owner.account_id)

                inventory = reopened.list_access_keys(
                    principal_id=principal.principal_id
                )

                self.assertEqual(len(inventory.access_keys), 2)
                self.assertIsNone(inventory.next_continuation_token)

                for entry in inventory.access_keys:
                    self.assertFalse(hasattr(entry, "secret_access_key"))

                    reopened.delete_access_key(
                        principal_id=principal.principal_id,
                        access_key_id=entry.access_key_id,
                    )

                reopened.delete_principal_policy(principal_id=principal.principal_id)
                reopened.delete_principal_policy(principal_id=principal.principal_id)

                with self.assertRaises(StorageError) as missing:
                    reopened.get_principal_policy(principal_id=principal.principal_id)

                self.assertEqual(missing.exception.code, "NoSuchPrincipalPolicy")
                reopened.delete_principal(principal_id=principal.principal_id)
                replacement = reopened.create_principal(name="app")
                self.assertNotEqual(replacement.principal_id, principal.principal_id)

    def test_pages_validation_and_committed_peer_changes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-admin-peer-") as temporary:
            path = Path(temporary) / "store"

            with Store.open(directory=path) as store:
                principals = [
                    store.create_principal(name=name) for name in ("a", "b", "c")
                ]

                first = store.list_principals(max_results=1)
                token = first.next_continuation_token
                self.assertIsNotNone(token)
                second = store.list_principals(max_results=2, continuation_token=token)
                self.assertIsNone(second.next_continuation_token)

                self.assertEqual(
                    [
                        principal.principal_id
                        for principal in first.principals + second.principals
                    ],
                    sorted(principal.principal_id for principal in principals),
                )

                for invalid in (0, 1001, 2**64):
                    with self.subTest(max_results=invalid):
                        with self.assertRaises(StorageError) as rejected:
                            store.list_principals(max_results=invalid)

                        self.assertEqual(rejected.exception.code, "InvalidArgument")

                with self.assertRaises(TypeError):
                    store.list_principals(max_results=True)

                with self.assertRaises(StorageError) as duplicate:
                    store.create_principal(name="a")

                self.assertEqual(duplicate.exception.code, "PrincipalAlreadyExists")
                self.assertEqual(duplicate.exception.mutation_outcome, "NotCommitted")
                principal = principals[0]

                script = """
import json
import sys

from mikrio import Store

with Store.open(directory=sys.argv[1]) as store:
    principal = store.set_principal_enabled(principal_id=sys.argv[2], is_enabled=False)

    print(json.dumps({"id": principal.principal_id, "enabled": principal.is_enabled}))
"""

                observation = json.loads(
                    subprocess.check_output(
                        [
                            sys.executable,
                            "-I",
                            "-c",
                            script,
                            str(path),
                            principal.principal_id,
                        ],
                        text=True,
                        timeout=20,
                    )
                )

                self.assertEqual(
                    observation, {"id": principal.principal_id, "enabled": False}
                )

                self.assertFalse(
                    store.get_principal(principal_id=principal.principal_id).is_enabled
                )
