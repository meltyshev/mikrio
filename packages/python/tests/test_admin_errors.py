"""Installed administration values, missing identities and lifecycle outcomes."""

from __future__ import annotations

import tempfile
import unittest
from collections.abc import Callable
from pathlib import Path

from mikrio import StorageError, Store

POLICY = (
    '{"Version":"2012-10-17",'
    '"Statement":{"Effect":"Allow",'
    '"Action":"s3:GetObject",'
    '"Resource":"arn:aws:s3:::photos/*"}}'
)


def get_operations(
    store: Store, principal_id: str, access_key_id: str
) -> list[tuple[str, Callable[[], object], str | None]]:
    return [
        ("owner", store.get_owner_info, None),
        ("principals", store.list_principals, None),
        (
            "create_principal",
            lambda: store.create_principal(name="candidate"),
            "NotCommitted",
        ),
        ("principal", lambda: store.get_principal(principal_id=principal_id), None),
        (
            "enable",
            lambda: store.set_principal_enabled(
                principal_id=principal_id, is_enabled=False
            ),
            "NotCommitted",
        ),
        (
            "delete_principal",
            lambda: store.delete_principal(principal_id=principal_id),
            "NotCommitted",
        ),
        (
            "put_policy",
            lambda: store.put_principal_policy(
                principal_id=principal_id, document=POLICY
            ),
            "NotCommitted",
        ),
        (
            "get_policy",
            lambda: store.get_principal_policy(principal_id=principal_id),
            None,
        ),
        (
            "delete_policy",
            lambda: store.delete_principal_policy(principal_id=principal_id),
            "NotCommitted",
        ),
        (
            "create_key",
            lambda: store.create_access_key(principal_id=principal_id),
            "NotCommitted",
        ),
        ("keys", lambda: store.list_access_keys(principal_id=principal_id), None),
        (
            "delete_key",
            lambda: store.delete_access_key(
                principal_id=principal_id, access_key_id=access_key_id
            ),
            "NotCommitted",
        ),
    ]


class AdministrationErrorTests(unittest.TestCase):
    def test_missing_identities_and_closed_store_preserve_each_operation_outcome(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-admin-errors-") as temporary:
            directory = Path(temporary)

            with Store.open(directory=directory) as store:
                principal = store.create_principal(name="removed")
                principal_id = principal.principal_id
                key = store.create_access_key(principal_id=principal_id)
                access_key_id = key.access_key.access_key_id

                store.delete_access_key(
                    principal_id=principal_id, access_key_id=access_key_id
                )

                store.delete_principal(principal_id=principal_id)
                calls = get_operations(store, principal_id, access_key_id)
                self.assertEqual(len(calls), 12)

                for name, operation, outcome in calls[3:]:
                    with self.subTest(operation=name, state="missing principal"):
                        with self.assertRaises(StorageError, msg=name) as failed:
                            operation()

                        self.assertEqual(failed.exception.code, "NoSuchPrincipal", name)

                        self.assertEqual(
                            failed.exception.mutation_outcome, outcome, name
                        )

                store.close()

                for name, operation, outcome in calls:
                    with self.subTest(operation=name, state="closed store"):
                        with self.assertRaises(StorageError, msg=name) as failed:
                            operation()

                        self.assertEqual(failed.exception.code, "StoreClosed", name)

                        self.assertEqual(
                            failed.exception.mutation_outcome, outcome, name
                        )

            with Store.open(directory=directory) as reopened:
                self.assertEqual(reopened.list_principals().principals, ())

    def test_utf8_name_boundary_case_and_normalization_preserve_distinct_identities(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="mikrio-admin-names-") as temporary:
            directory = Path(temporary)

            with Store.open(directory=directory) as store:
                names = ("é" * 64, "App", "app", "é", "e\u0301")
                principals = [store.create_principal(name=name) for name in names]

                self.assertEqual(
                    len({principal.principal_id for principal in principals}),
                    len(names),
                )

                self.assertEqual(
                    tuple(principal.name for principal in principals), names
                )

                for name in ("", "é" * 65, "a\x00b", "a\nb", "a\x7fb"):
                    with self.subTest(name=name):
                        with self.assertRaises(StorageError, msg=repr(name)) as failed:
                            store.create_principal(name=name)

                        self.assertEqual(failed.exception.code, "InvalidArgument")

                        self.assertEqual(
                            failed.exception.mutation_outcome, "NotCommitted"
                        )

                with self.assertRaises(TypeError):
                    store.set_principal_enabled(
                        principal_id=principals[0].principal_id,
                        is_enabled=1,  # type: ignore[arg-type]
                    )

            with Store.open(directory=directory) as reopened:
                inventory = reopened.list_principals()

                self.assertEqual(
                    {principal.name for principal in inventory.principals}, set(names)
                )

                self.assertTrue(
                    all(principal.is_enabled for principal in inventory.principals)
                )
