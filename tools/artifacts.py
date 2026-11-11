"""Shared paths and checksums for distribution tools."""

from __future__ import annotations

import hashlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def digest(path: Path) -> str:
    """Return the SHA-256 of a file without loading it into memory."""

    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()
