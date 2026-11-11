"""Exercise two installed extension images sharing one SQLite runtime."""

from __future__ import annotations

import ctypes
import importlib.util
import os
import shutil
import struct
import sys
from contextlib import ExitStack
from pathlib import Path
from types import ModuleType


def load_package(name: str, directory: Path) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, directory / "__init__.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def sqlite_address(image: ctypes.CDLL) -> int:
    name = b"mikrio_sqlite3_v1_sqlite3_open_v2"

    if os.name != "nt":
        address = ctypes.cast(getattr(image, name.decode()), ctypes.c_void_p).value
        assert address is not None
        return address

    base = image._handle
    dos = ctypes.string_at(base, 64)
    assert dos[:2] == b"MZ"
    nt_offset = struct.unpack_from("<I", dos, 60)[0]
    headers = ctypes.string_at(base + nt_offset, 264)
    assert headers[:4] == b"PE\0\0"
    assert struct.unpack_from("<H", headers, 24)[0] == 0x20B
    # PE32+ optional headers follow the signature and 20-byte COFF header.
    image_size = struct.unpack_from("<I", headers, 24 + 56)[0]

    def read(offset: int, length: int) -> bytes:
        assert 0 <= offset <= image_size and 0 <= length <= image_size - offset
        return ctypes.string_at(base + offset, length)

    imports_offset, imports_length = struct.unpack_from("<II", headers, 24 + 120)

    for offset in range(imports_offset, imports_offset + imports_length, 20):
        names_offset, _, _, library_offset, addresses_offset = struct.unpack(
            "<IIIII", read(offset, 20)
        )

        if not library_offset:
            break

        if (
            read(library_offset, len(b"mikrio_sqlite3.dll\0"))
            != b"mikrio_sqlite3.dll\0"
        ):
            continue

        for index in range(image_size // 8):
            name_offset = struct.unpack("<Q", read(names_offset + 8 * index, 8))[0]

            if not name_offset:
                break

            if name_offset & (1 << 63):
                continue

            if read(name_offset + 2, len(name) + 1) == name + b"\0":
                address = struct.unpack("<Q", read(addresses_offset + 8 * index, 8))[0]
                assert address != 0
                return address

    raise AssertionError("native image does not import the shared SQLite runtime")


def main() -> None:
    order, temporary = sys.argv[1:]
    assert order in {"forward", "reverse"}
    root = Path(temporary)
    root.mkdir()
    spec = importlib.util.find_spec("mikrio")
    assert spec is not None and spec.origin is not None
    installed = Path(spec.origin).parent
    replica = root / "replica"
    shutil.copytree(installed, replica)
    directories = [installed, replica]

    if order == "reverse":
        directories.reverse()

    first, second = [
        load_package(f"mikrio_image_{index}", directory)
        for index, directory in enumerate(directories)
    ]

    assert first.Store is not second.Store
    native = [sys.modules[f"{module.__name__}._native"] for module in (first, second)]
    assert native[0].__file__ is not None and native[1].__file__ is not None
    assert Path(native[0].__file__).resolve() != Path(native[1].__file__).resolve()
    images = [ctypes.CDLL(module.__file__) for module in native]

    symbols = [sqlite_address(image) for image in images]

    assert symbols[0] == symbols[1]

    directory = root / "store"

    with ExitStack() as owners:
        writer = owners.enter_context(first.Store.open(directory=directory))
        peer = owners.enter_context(second.Store.open(directory=directory))
        assert writer.info.store_id == peer.info.store_id
        assert writer.info.directory == peer.info.directory
        writer.create_bucket(bucket="bucket")
        writer.put_object(bucket="bucket", key="held", body=b"detached")
        selected = writer.get_object(bucket="bucket", key="held")
        held = owners.enter_context(selected.body)
        peer.delete_object(bucket="bucket", key="held")
        peer.close()
        writer.put_object(bucket="bucket", key="acknowledged", body=b"survives")

        assert (
            writer.head_object(bucket="bucket", key="acknowledged").content_length == 8
        )

        reopened = owners.enter_context(second.Store.open(directory=directory))

        assert (
            reopened.head_object(bucket="bucket", key="acknowledged").content_length
            == 8
        )

        with reopened.get_object(bucket="bucket", key="acknowledged").body as body:
            assert body.read_all(max_bytes=8) == b"survives"

        assert held.read_all(max_bytes=8) == b"detached"
        held.close()
        writer.close()
        reopened.close()
        assert selected.content_length == 8

    print(f"PASS distinct_images_{order}")


if __name__ == "__main__":
    main()
