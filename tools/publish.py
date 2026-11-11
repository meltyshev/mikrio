"""Publish verified release archives; never compile or rebuild package contents."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import shutil
import struct
import subprocess
import tempfile
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

from tools.artifacts import digest
from tools.release import read_tar_members, rust_publish_order, verify, write_output

USER_AGENT = "mikrio-release (https://github.com/meltyshev/mikrio)"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    """Never forward publication credentials or payloads to a different URL."""

    def redirect_request(
        self, req: Any, fp: Any, code: int, msg: str, headers: Any, newurl: str
    ) -> None:
        return None


def registry_request(
    url: str, *, body: bytes | None = None, token: str | None = None
) -> bytes | None:
    headers = {"User-Agent": USER_AGENT, "Accept": "application/json"}

    if token:
        headers["Authorization"] = token

    if body is not None:
        headers["Content-Type"] = "application/octet-stream"

    request = urllib.request.Request(
        url, data=body, headers=headers, method="GET" if body is None else "PUT"
    )

    try:
        # PUT is deliberately not retried: an interrupted upload can have committed.
        with urllib.request.build_opener(NoRedirect()).open(
            request, timeout=60
        ) as response:
            return bytes(response.read())
    except urllib.error.HTTPError as error:
        if error.code == 404 and body is None:
            return None

        raise RuntimeError(f"registry returned HTTP {error.code}: {url}") from None


def find_published_crate(name: str, version: str) -> dict[str, Any] | None:
    response = registry_request(
        f"https://index.crates.io/{name[:2]}/{name[2:4]}/{name}"
    )

    if response is None:
        return None

    for line in response.splitlines():
        package: dict[str, Any] = json.loads(line)

        if package["vers"] != version:
            continue

        if package.get("yanked"):
            raise ValueError(f"refusing yanked dependency {name} {version}")

        return package

    return None


def reuse_crates(directory: Path) -> None:
    """Reuse unchanged versions, allowing only Git provenance to differ."""

    for package in rust_publish_order():
        published = find_published_crate(package["name"], package["version"])

        if published is None:
            continue

        path = directory / package["file"]

        if digest(path) == published["cksum"]:
            continue

        payload = registry_request(
            f"https://static.crates.io/crates/{package['name']}/{package['file']}"
        )

        if payload is None or hashlib.sha256(payload).hexdigest() != published["cksum"]:
            raise ValueError("published crate checksum mismatch")

        with tempfile.TemporaryDirectory(prefix="mikrio-published-crate-") as temporary:
            downloaded = Path(temporary) / path.name
            downloaded.write_bytes(payload)
            local_contents = read_tar_members(path)
            published_contents = read_tar_members(downloaded)
            prefix = f"{package['name']}-{package['version']}/"
            local_contents.pop(prefix + ".cargo_vcs_info.json", None)
            published_contents.pop(prefix + ".cargo_vcs_info.json", None)

            if local_contents != published_contents:
                raise ValueError(
                    f"published crate source changed without a version bump: {path.name}"
                )

            shutil.copyfile(downloaded, path)


def crate_upload_metadata(path: Path) -> dict[str, Any]:
    """Translate Cargo's normalized archive manifest to its documented upload API."""
    contents = read_tar_members(path)
    prefix = path.name.removesuffix(".crate") + "/"
    manifest = tomllib.loads(contents[prefix + "Cargo.toml"].decode())
    package = manifest["package"]
    dependencies = encode_crate_dependencies(manifest)

    metadata = {
        key: package.get(key)
        for key in (
            "description",
            "documentation",
            "homepage",
            "license",
            "repository",
            "links",
        )
    }

    readme = package.get("readme")

    metadata.update(
        {
            "name": package["name"],
            "vers": package["version"],
            "deps": dependencies,
            "features": manifest.get("features", {}),
            "authors": package.get("authors", []),
            "keywords": package.get("keywords", []),
            "categories": package.get("categories", []),
            "badges": manifest.get("badges", {}),
            "license_file": package.get("license-file"),
            "rust_version": package.get("rust-version"),
            "readme_file": readme if isinstance(readme, str) else None,
            "readme": contents[prefix + readme].decode()
            if isinstance(readme, str)
            else None,
        }
    )

    return metadata


def encode_crate_dependencies(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    dependencies = []

    for target, table in [(None, manifest), *manifest.get("target", {}).items()]:
        for key, kind in (
            ("dependencies", "normal"),
            ("build-dependencies", "build"),
            ("dev-dependencies", "dev"),
        ):
            for alias, dependency in table.get(key, {}).items():
                dependencies.append(
                    encode_crate_dependency(alias, dependency, target, kind)
                )

    return dependencies


def encode_crate_dependency(
    alias: str, dependency: str | dict[str, Any], target: str | None, kind: str
) -> dict[str, Any]:
    fields = {"version": dependency} if isinstance(dependency, str) else dependency
    unresolved_sources = {"path", "git", "registry", "registry-index", "workspace"}

    if unresolved_sources.intersection(fields):
        raise ValueError("crate contains an unresolved or non-crates.io dependency")

    return {
        "name": fields.get("package", alias),
        "version_req": fields["version"],
        "features": fields.get("features", []),
        "optional": fields.get("optional", False),
        "default_features": fields.get("default-features", True),
        "target": target,
        "kind": kind,
        "registry": None,
        "explicit_name_in_toml": alias if "package" in fields else None,
    }


def require_checksum(actual: str, expected: str, name: str) -> None:
    if actual != expected:
        raise ValueError(
            f"registry has different bytes for {name}; do not overwrite or rebuild this version"
        )


def wait_for_crate(package: dict[str, str]) -> None:
    for _ in range(30):
        published = find_published_crate(package["name"], package["version"])

        if published is not None:
            require_checksum(published["cksum"], package["sha256"], package["file"])
            return

        time.sleep(10)

    raise TimeoutError(f"crate is not visible in the index: {package['file']}")


def publish_crates(directory: Path, records: list[dict[str, str]]) -> None:
    packages = {
        package["file"]: package for package in records if package["kind"] == "crates"
    }

    for ordered in rust_publish_order():
        package = packages[ordered["file"]]
        published = find_published_crate(package["name"], package["version"])

        if published is not None:
            require_checksum(published["cksum"], package["sha256"], package["file"])
            continue

        token = os.environ.get("CARGO_REGISTRY_TOKEN")

        if not token:
            raise ValueError(
                "CARGO_REGISTRY_TOKEN is required for missing crate versions"
            )

        path = directory / package["file"]

        metadata = json.dumps(
            crate_upload_metadata(path), separators=(",", ":")
        ).encode()

        archive = path.read_bytes()

        body = (
            struct.pack("<I", len(metadata))
            + metadata
            + struct.pack("<I", len(archive))
            + archive
        )

        print(f"Publishing {package['name']} {package['version']}", flush=True)

        response = registry_request(
            "https://crates.io/api/v1/crates/new", body=body, token=token
        )

        if response is None:
            raise ValueError("missing crates.io publication response")

        result = json.loads(response)

        if result.get("errors"):
            raise ValueError(
                f"crates.io rejected {package['file']}: {result['errors']}"
            )

        if result.get("warnings"):
            print(json.dumps(result["warnings"]))

        wait_for_crate(package)


def is_npm_published(package: dict[str, str], directory: Path) -> bool:
    name = urllib.parse.quote(package["name"], safe="")

    response = registry_request(
        f"https://registry.npmjs.org/{name}/{package['version']}"
    )

    if response is None:
        return False

    data = json.loads(response)

    expected = (
        "sha512-"
        + base64.b64encode(
            hashlib.sha512((directory / package["file"]).read_bytes()).digest()
        ).decode()
    )

    require_checksum(data["dist"]["integrity"], expected, package["file"])
    return True


def publish_npm(directory: Path, records: list[dict[str, str]]) -> None:
    packages = sorted(
        (package for package in records if package["kind"] == "npm"),
        key=lambda package: (package["name"] == "mikrio", package["name"]),
    )

    for package in packages:
        if is_npm_published(package, directory):
            continue

        subprocess.run(
            [
                "npm",
                "publish",
                str((directory / package["file"]).resolve()),
                "--access",
                "public",
                "--ignore-scripts",
                "--registry",
                "https://registry.npmjs.org/",
                "--tag",
                "latest",
            ],
            check=True,
        )

        for _ in range(30):
            if is_npm_published(package, directory):
                break

            time.sleep(10)
        else:
            raise TimeoutError(f"npm package is not visible: {package['file']}")


def pypi_checksums(version: str) -> dict[str, str]:
    response = registry_request(f"https://pypi.org/pypi/mikrio/{version}/json")

    if response is None:
        return {}

    files = json.loads(response)["urls"]

    if any(package.get("yanked") for package in files):
        raise ValueError("refusing a yanked PyPI release")

    return {package["filename"]: package["digests"]["sha256"] for package in files}


def prepare_pypi(
    directory: Path, records: list[dict[str, str]], destination: Path
) -> None:
    packages = [package for package in records if package["kind"] == "pypi"]
    existing = pypi_checksums(packages[0]["version"])

    if set(existing) - {package["file"] for package in packages}:
        raise ValueError("PyPI contains unexpected files for this version")

    destination.mkdir(parents=True, exist_ok=False)

    for package in packages:
        if package["file"] in existing:
            require_checksum(
                existing[package["file"]], package["sha256"], package["file"]
            )

            continue

        shutil.copyfile(directory / package["file"], destination / package["file"])

    write_output("pending", str(any(destination.iterdir())).lower())


def verify_registries(directory: Path, records: list[dict[str, str]]) -> None:
    python_packages = [package for package in records if package["kind"] == "pypi"]

    for _ in range(30):
        existing = pypi_checksums(python_packages[0]["version"])

        if set(existing) == {package["file"] for package in python_packages}:
            break

        time.sleep(10)
    else:
        raise ValueError("PyPI does not contain the complete release")

    for package in records:
        if package["kind"] == "pypi":
            require_checksum(
                existing[package["file"]], package["sha256"], package["file"]
            )
        elif package["kind"] == "crates":
            wait_for_crate(package)
        elif package["kind"] == "npm" and not is_npm_published(package, directory):
            raise ValueError(f"npm is missing {package['file']}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)

    parser.add_argument(
        "action", choices=("reuse-crates", "crates", "npm", "prepare-pypi", "verify")
    )

    parser.add_argument("directory", type=Path)
    parser.add_argument("--output", type=Path, default=Path("pypi-upload"))
    arguments = parser.parse_args()

    if arguments.action == "reuse-crates":
        reuse_crates(arguments.directory)
        return

    records = verify(arguments.directory)

    if arguments.action == "crates":
        publish_crates(arguments.directory, records)
    elif arguments.action == "npm":
        publish_npm(arguments.directory, records)
    elif arguments.action == "prepare-pypi":
        prepare_pypi(arguments.directory, records, arguments.output)
    else:
        verify_registries(arguments.directory, records)


if __name__ == "__main__":
    main()
