"""Show, check or set the coordinated package version without publishing."""

from __future__ import annotations

import argparse
import json
import re
import tomllib
from pathlib import Path
from typing import Any

from tools.artifacts import ROOT


def validate(value: str) -> str:
    """Accept release versions with the same spelling in Cargo, npm and Python."""

    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value):
        raise ValueError("use a release version in MAJOR.MINOR.PATCH form")

    return value


def current(root: Path = ROOT) -> str:
    value = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))[
        "workspace"
    ]["package"]["version"]

    if not isinstance(value, str):
        raise ValueError("workspace.package.version must be a string")

    return validate(value)


def replace_version(text: str, value: str) -> str:
    result, count = re.subn(
        r'(?m)^(\s*version\s*=\s*)"[^"\n]*"',
        lambda match: match[1] + json.dumps(value),
        text,
    )

    if count != 1:
        raise ValueError("expected one literal version field")

    return result


def dependency_tables(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    tables = [manifest, manifest.get("workspace", {})]
    tables.extend(manifest.get("target", {}).values())

    return [
        table[kind]
        for table in tables
        for kind in ("dependencies", "dev-dependencies", "build-dependencies")
        if kind in table
    ]


def local_dependencies(
    manifest: dict[str, Any], directory: Path, crates: dict[Path, str]
) -> list[dict[str, Any]]:
    result = []

    for table in dependency_tables(manifest):
        for alias, dependency in table.items():
            entry = dependency if isinstance(dependency, dict) else {}
            name = entry.get("package", alias)

            if entry.get("workspace"):
                continue

            if "path" not in entry:
                if name in crates.values():
                    raise ValueError(
                        f"{directory}: {name} needs a local workspace path"
                    )

                continue

            path = (directory / entry["path"] / "Cargo.toml").resolve()

            if name in crates.values() and path not in crates:
                raise ValueError(f"{directory}: {name} points outside the workspace")

            if path in crates:
                result.append(entry)

    return result


def update_manifest_dependencies(
    text: str, path: Path, crates: dict[Path, str], version: str
) -> str:
    """Retain TOML comments and formatting while updating local inline pins."""

    def replace_dependency(match: re.Match[str]) -> str:
        entry = next(iter(tomllib.loads(match[0]).values()))

        if "path" not in entry:
            return match[0]

        dependency = (path.parent / entry["path"] / "Cargo.toml").resolve()

        if dependency not in crates:
            return match[0]

        return re.sub(
            r'(\bversion\s*=\s*)"[^"\n]*"',
            lambda field: field[1] + json.dumps("=" + version),
            match[0],
            count=1,
        )

    expected = tomllib.loads(text)

    for dependency in local_dependencies(expected, path.parent, crates):
        dependency["version"] = "=" + version

    # Restrict textual edits to dependency tables, then verify their full TOML
    # meaning. Unsupported layouts fail before writing instead of changing metadata.
    sections = re.split(r"(?m)(^\[[^\n]+\][ \t]*(?:#.*)?$)", text)

    for index in range(1, len(sections), 2):
        if not dependency_tables(tomllib.loads(sections[index])):
            continue

        sections[index + 1] = re.sub(
            r"(?m)^[ \t]*[\w-]+\s*=\s*\{[^\n]*\}[ \t]*(?:#.*)?$",
            replace_dependency,
            sections[index + 1],
        )

    updated = "".join(sections)

    if tomllib.loads(updated) != expected:
        raise ValueError(
            f"{path}: local dependencies need an inline exact version; "
            "unrelated TOML content must be preserved"
        )

    return updated


def update_cargo_lock(text: str, names: set[str], version: str) -> str:
    """Change only local package identities and their qualified references."""
    blocks = re.split(r"(?m)(?=^\[\[package\]\])", text)
    found: list[str] = []
    references = {}

    for index, block in enumerate(blocks):
        if not block.startswith("[[package]]"):
            continue

        package = tomllib.loads(block)["package"][0]
        name = package["name"]

        if name not in names or "source" in package:
            continue

        found.append(name)
        references[f"{name} {package['version']}"] = f"{name} {version}"
        blocks[index] = replace_version(block, version)

    if set(found) != names or len(found) != len(names):
        raise ValueError("Cargo.lock must contain each workspace package exactly once")

    updated = "".join(blocks)

    for before, after in references.items():
        updated = updated.replace(json.dumps(before), json.dumps(after))

    expected = tomllib.loads(text)

    for package in expected["package"]:
        if package["name"] in names and "source" not in package:
            package["version"] = version

        if "dependencies" in package:
            package["dependencies"] = [
                references.get(dependency, dependency)
                for dependency in package["dependencies"]
            ]

    if tomllib.loads(updated) != expected:
        raise ValueError("Cargo.lock update would change unrelated metadata")

    return updated


def prepare_updates(root: Path, version: str) -> dict[Path, str]:
    """Prepare and validate every edit before changing any file."""
    validate(version)
    root = root.resolve()
    manifest_path = root / "Cargo.toml"
    manifest = manifest_path.read_text(encoding="utf-8")
    expected_manifest = tomllib.loads(manifest)
    workspace = expected_manifest["workspace"]
    crates = {}

    for pattern in workspace["members"]:
        members = sorted(root.glob(pattern))

        if not members:
            raise ValueError(f"workspace member does not exist: {pattern}")

        for directory in members:
            path = (directory / "Cargo.toml").resolve()

            if not path.is_relative_to(root):
                raise ValueError("workspace members must be inside the repository")

            package = tomllib.loads(path.read_text(encoding="utf-8"))["package"]

            if package.get("version") != {"workspace": True}:
                raise ValueError(f"{path}: use version.workspace = true")

            crates[path] = package["name"]

    if not crates or len(set(crates.values())) != len(crates):
        raise ValueError("workspace package names must be nonempty and unique")

    changed, count = re.subn(
        r"(?ms)(^\[workspace\.package\][^\n]*\n)(.*?)(?=^\[|\Z)",
        lambda match: match[1] + replace_version(match[2], version),
        manifest,
    )

    if count != 1:
        raise ValueError("expected one workspace.package table")

    workspace["package"]["version"] = version

    if tomllib.loads(changed) != expected_manifest:
        raise ValueError("workspace version update would change unrelated TOML content")

    result = {manifest_path: changed}

    for path in [manifest_path, *crates]:
        text = result.get(path, path.read_text(encoding="utf-8"))
        result[path] = update_manifest_dependencies(text, path, crates, version)

    cargo_lock_path = root / "Cargo.lock"

    result[cargo_lock_path] = update_cargo_lock(
        cargo_lock_path.read_text(encoding="utf-8"), set(crates.values()), version
    )

    python_manifest_path = root / "packages/python/pyproject.toml"
    metadata = tomllib.loads(python_manifest_path.read_text(encoding="utf-8"))
    project = metadata["project"]

    if "version" in project or "version" not in project.get("dynamic", []):
        raise ValueError(
            'Python must inherit the Cargo version with dynamic = ["version"]'
        )

    binding_manifest_path = (
        python_manifest_path.parent / metadata["tool"]["maturin"]["manifest-path"]
    ).resolve()

    if binding_manifest_path not in crates:
        raise ValueError("Python must build a version-inheriting workspace crate")

    locked_python = tomllib.loads(
        (python_manifest_path.parent / "uv.lock").read_text(encoding="utf-8")
    )

    local_packages = [
        package
        for package in locked_python["package"]
        if package["name"] == project["name"]
        and package.get("source") == {"editable": "."}
    ]

    if len(local_packages) != 1 or "version" in local_packages[0]:
        raise ValueError("refresh packages/python/uv.lock for dynamic project metadata")

    node_manifest_path = root / "packages/node/package.json"
    package = json.loads(node_manifest_path.read_text(encoding="utf-8"))

    targets = json.loads(
        (root / "packages/node/src/targets.json").read_text(encoding="utf-8")
    )

    expected = {target["name"]: version for target in targets}

    matches_native_targets = package["name"] == "mikrio" and set(
        package["optionalDependencies"]
    ) == set(expected)

    if not matches_native_targets:
        raise ValueError("Node package dependencies must match the native target list")

    if package["version"] != version or package["optionalDependencies"] != expected:
        package["version"] = version
        package["optionalDependencies"] = expected
        result[node_manifest_path] = json.dumps(package, indent=2) + "\n"

    return {
        path: text
        for path, text in result.items()
        if path.read_text(encoding="utf-8") != text
    }


def check(root: Path = ROOT) -> str:
    version = current(root)
    pending = prepare_updates(root, version)

    if pending:
        paths = ", ".join(str(path.relative_to(root.resolve())) for path in pending)

        raise ValueError(
            f"version metadata differs from Cargo.toml ({version}): {paths}"
        )

    return version


def set_version(version: str, root: Path = ROOT) -> list[Path]:
    pending = prepare_updates(root, version)
    originals = {path: path.read_bytes() for path in pending}
    written = []

    try:
        for path, text in pending.items():
            written.append(path)
            path.write_text(text, encoding="utf-8", newline="\n")

        check(root)
    except BaseException as error:
        failures = []

        for path in written:
            try:
                path.write_bytes(originals[path])
            except OSError as restore_error:
                failures.append(f"{path}: {restore_error}")

        if failures:
            raise OSError(
                "could not restore version files: " + "; ".join(failures)
            ) from error

        raise

    return written


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("show", help="print the authoritative workspace version")
    commands.add_parser("check", help="reject inconsistent package version metadata")
    setter = commands.add_parser("set", help="update release metadata locally")
    setter.add_argument("version", help="MAJOR.MINOR.PATCH")
    arguments = parser.parse_args()

    try:
        if arguments.command == "show":
            print(current())
        elif arguments.command == "check":
            print(f"Package versions agree: {check()}")
        else:
            changed = set_version(arguments.version)
            print(f"Package version {arguments.version}; updated {len(changed)} files.")

            for path in changed:
                print(path.relative_to(ROOT))
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
