"""Reject unreviewed Rust archives before their build scripts can execute."""

from __future__ import annotations

import datetime
import unittest
from typing import Any

from tools import dependencies


class DependencyTests(unittest.TestCase):
    def test_release_accepts_age_boundary_and_rejects_unverified_archives(self) -> None:
        package = dependencies.CargoPackage("example", "1.2.3", "a" * 64)
        now = datetime.datetime(2020, 1, 4, tzinfo=datetime.UTC)

        release: dict[str, Any] = {
            "num": "1.2.3",
            "checksum": "a" * 64,
            "yanked": False,
            "created_at": "2020-01-01T00:00:00Z",
        }

        dependencies.check_release(package, release, now)

        rejected_fields = [
            ("num", "1.2.4", "wrong registry version"),
            ("checksum", "b" * 64, "registry checksum mismatch"),
            ("yanked", True, "yanked release"),
            ("created_at", "2020-01-01T00:00:01Z", "less than 72 hours"),
            ("created_at", "2020-02-01T00:00:00Z", "less than 72 hours"),
            ("created_at", "2020-01-01", "missing publication timezone"),
        ]

        for field, value, message in rejected_fields:
            changed_release = {**release, field: value}

            with self.subTest(field=field, value=value):
                with self.assertRaisesRegex(ValueError, message):
                    dependencies.check_release(package, changed_release, now)


if __name__ == "__main__":
    unittest.main()
