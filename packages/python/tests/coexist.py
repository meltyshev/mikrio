"""Load the admitted interpreter SQLite and installed engine in both orders."""

from __future__ import annotations

import importlib
import sys
from contextlib import ExitStack
from pathlib import Path


def main() -> None:
    order, path = sys.argv[1:]
    assert order in {"sqlite-first", "engine-first"}

    with ExitStack() as cleanup:
        if order == "sqlite-first":
            sqlite = importlib.import_module("sqlite3")
            database = sqlite.connect(":memory:")
            cleanup.callback(database.close)

        engine = importlib.import_module("mikrio")
        store = cleanup.enter_context(engine.Store.open(directory=Path(path)))

        if order == "engine-first":
            sqlite = importlib.import_module("sqlite3")
            database = sqlite.connect(":memory:")
            cleanup.callback(database.close)

        database.execute("CREATE TABLE separate_data(value BLOB)")

        database.execute(
            "INSERT INTO separate_data(value) VALUES (?)", (b"independent",)
        )

        store.create_bucket(bucket="coexist")
        store.put_object(bucket="coexist", key="object", body=b"persistent")

        assert database.execute("SELECT value FROM separate_data").fetchone() == (
            b"independent",
        )

    with engine.Store.open(directory=Path(path)) as reopened:
        with reopened.get_object(bucket="coexist", key="object").body as body:
            assert body.read_all(max_bytes=10) == b"persistent"

    print("PASS", order, sqlite.sqlite_version)


if __name__ == "__main__":
    main()
