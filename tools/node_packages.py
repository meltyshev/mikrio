"""Identify the public Node package and its matching native target package."""

from __future__ import annotations

import json
from typing import Any

from tools.artifacts import ROOT

PUBLIC_PACKAGE_NAME = "@mikrio/node"


def archive_name(name: str, version: str) -> str:
    return name.removeprefix("@").replace("/", "-") + f"-{version}.tgz"


def targets() -> list[dict[str, str]]:
    """Share the NAPI-RS target names with the lazy runtime loader."""

    result: list[dict[str, str]] = json.loads(
        (ROOT / "packages/node/src/targets.json").read_text(encoding="utf-8")
    )

    return result


def target_metadata(matrix: str) -> dict[str, str]:
    selected = [target for target in targets() if target["matrix"] == matrix]

    if len(selected) != 1:
        raise ValueError(f"unsupported or ambiguous Node target: {matrix}")

    return selected[0]


def optional_dependencies(version: str) -> dict[str, str]:
    return {target["name"]: version for target in targets()}


def native_readme(matrix: str) -> str:
    target = target_metadata(matrix)

    return f"""# {target["name"]}

Native Node.js binding for mikrio on `{matrix}`.

Install [{PUBLIC_PACKAGE_NAME}](https://www.npmjs.com/package/{PUBLIC_PACKAGE_NAME});
it selects this package automatically for the matching operating system and
architecture.

Keep `mikrio.node`, `{target["runtime"]}`, and `mikrio-sqlite-runtime.json`
together. No install script or local compiler is required.

See the [project documentation](https://github.com/meltyshev/mikrio) for the
API, supported platforms, and deployment requirements.

Licensed under Apache-2.0. Native dependencies have separate bundled notices.
"""


def native_manifest(
    version: str, matrix: str, engines: dict[str, str]
) -> dict[str, Any]:
    target = target_metadata(matrix)

    manifest = {
        "name": target["name"],
        "version": version,
        "description": f"Native Node.js binding for mikrio ({matrix})",
        "license": "Apache-2.0",
        "homepage": "https://github.com/meltyshev/mikrio#readme",
        "repository": {
            "type": "git",
            "url": "git+https://github.com/meltyshev/mikrio.git",
            "directory": "crates/mikrio-node",
        },
        "bugs": {"url": "https://github.com/meltyshev/mikrio/issues"},
        "main": "./mikrio.node",
        "exports": {".": "./mikrio.node", "./package.json": "./package.json"},
        "files": [
            "*.node",
            target["runtime"],
            "mikrio-sqlite-runtime.json",
            "LICENSE",
            "README.md",
            "THIRD_PARTY_LICENSES.txt",
        ],
        "os": [target["os"]],
        "cpu": [target["cpu"]],
    }

    if "libc" in target:
        manifest["libc"] = [target["libc"]]

    manifest["engines"] = engines
    manifest["publishConfig"] = {"access": "public"}

    return manifest
