"""Verify the age and integrity of locked Rust registry releases before builds."""

from __future__ import annotations

import concurrent.futures
import datetime
import json
import tomllib
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from tools.artifacts import ROOT

MINIMUM_RELEASE_AGE = datetime.timedelta(hours=72)
CARGO_REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


@dataclass(frozen=True)
class CargoPackage:
    """Identify the exact registry archive selected by Cargo."""

    name: str
    version: str
    checksum: str


def check(root: Path) -> None:
    """Check both lockfiles without downloading or executing dependency code."""
    packages: set[CargoPackage] = set()

    for path in [root / "Cargo.lock", root / "fuzz/Cargo.lock"]:
        lock = tomllib.loads(path.read_text(encoding="utf-8"))

        for package in lock["package"]:
            if "source" not in package:
                continue

            if package["source"] != CARGO_REGISTRY:
                raise ValueError(f"{path}: unexpected Cargo source")

            packages.add(
                CargoPackage(package["name"], package["version"], package["checksum"])
            )

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
        list(executor.map(verify_registry_release, packages))

    print(f"Verified release ages and checksums for {len(packages)} Rust inputs.")


def verify_registry_release(package: CargoPackage) -> None:
    """Read official metadata for the exact locked version."""
    url = f"https://crates.io/api/v1/crates/{package.name}/{package.version}"

    request = urllib.request.Request(
        url, headers={"User-Agent": "mikrio-dependency-check"}
    )

    with urllib.request.urlopen(request, timeout=45) as response:
        release = json.load(response)["version"]

    check_release(package, release, datetime.datetime.now(datetime.UTC))


def check_release(
    package: CargoPackage, release: dict[str, Any], now: datetime.datetime
) -> None:
    """Reject a mismatched, yanked, undated or insufficiently aged archive."""
    label = f"{package.name}@{package.version}"

    if release["num"] != package.version:
        raise ValueError(f"{label}: wrong registry version")

    if release["checksum"] != package.checksum:
        raise ValueError(f"{label}: registry checksum mismatch")

    if release["yanked"]:
        raise ValueError(f"{label}: yanked release")

    published = datetime.datetime.fromisoformat(
        release["created_at"].replace("Z", "+00:00")
    )

    if published.tzinfo is None:
        raise ValueError(f"{label}: missing publication timezone")

    if now - published < MINIMUM_RELEASE_AGE:
        raise ValueError(f"{label}: published less than 72 hours ago")


if __name__ == "__main__":
    check(ROOT)
