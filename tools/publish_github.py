"""Tag tested source and publish a GitHub release with verified immutable assets."""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path
from typing import Any

from tools.artifacts import digest
from tools.release import REPOSITORY, source_revision, verify
from tools.version import check


def read_api(endpoint: str) -> dict[str, Any]:
    result: dict[str, Any] = json.loads(
        subprocess.check_output(
            ["gh", "api", f"repos/{REPOSITORY}/{endpoint}"], encoding="utf-8"
        )
    )

    return result


def run_release_command(*arguments: str) -> None:
    subprocess.run(["gh", "release", *arguments, "--repo", REPOSITORY], check=True)


def find_release(tag: str) -> dict[str, Any] | None:
    # Direct GraphQL lookup includes pending draft tags without the replication
    # lag of REST release listings: https://github.com/cli/cli/issues/6599.
    owner, name = REPOSITORY.split("/", 1)

    query = """
        query($owner: String!, $name: String!, $tag: String!) {
            repository(owner: $owner, name: $name) {
                release(tagName: $tag) { databaseId }
            }
        }
    """

    response = json.loads(
        subprocess.check_output(
            [
                "gh",
                "api",
                "graphql",
                "--raw-field",
                "query=" + query,
                "--raw-field",
                "owner=" + owner,
                "--raw-field",
                "name=" + name,
                "--raw-field",
                "tag=" + tag,
            ],
            encoding="utf-8",
        )
    )

    reference = response["data"]["repository"]["release"]

    if reference is None:
        return None

    release = read_api(f"releases/{reference['databaseId']}")

    if release["tag_name"] != tag:
        raise ValueError("GitHub release differs from the requested tag")

    return release


def create_tag(directory: Path) -> None:
    verify(directory)
    tag = "v" + check()

    references = json.loads(
        subprocess.check_output(
            ["gh", "api", f"repos/{REPOSITORY}/git/matching-refs/tags/{tag}"],
            encoding="utf-8",
        )
    )

    if not any(reference["ref"] == f"refs/tags/{tag}" for reference in references):
        subprocess.run(
            [
                "gh",
                "api",
                f"repos/{REPOSITORY}/git/refs",
                "--method",
                "POST",
                "--raw-field",
                f"ref=refs/tags/{tag}",
                "--raw-field",
                f"sha={source_revision()}",
            ],
            check=True,
        )

    verify_tag(tag)


def publish(directory: Path) -> None:
    verify(directory)
    tag = "v" + check()
    verify_tag(tag)

    release = find_release(tag)

    if release is None:
        run_release_command(
            "create",
            tag,
            "--draft",
            "--verify-tag",
            "--target",
            source_revision(),
            "--title",
            tag,
            "--notes-file",
            str(directory / "release-notes.md"),
        )

        release = find_release(tag)

    if release is None:
        raise ValueError("created GitHub draft could not be found")

    expected = {path.name: digest(path) for path in directory.iterdir()}
    assets = {asset["name"]: asset for asset in release["assets"]}

    if len(assets) != len(release["assets"]) or set(assets) - set(expected):
        raise ValueError("GitHub release contains unexpected assets")

    for name, asset in assets.items():
        if (
            asset["state"] != "uploaded"
            or asset.get("digest") != "sha256:" + expected[name]
        ):
            raise ValueError(f"GitHub asset has different bytes: {name}")

    missing = sorted(set(expected) - set(assets))

    if missing and not release["draft"]:
        raise ValueError("published release is missing assets; refusing to modify it")

    for name in missing:
        run_release_command("upload", tag, str(directory / name))

    final = read_api(f"releases/{release['id']}")

    actual = {
        asset["name"]: asset.get("digest")
        for asset in final["assets"]
        if asset["state"] == "uploaded"
    }

    if len(final["assets"]) != len(actual) or actual != {
        name: "sha256:" + value for name, value in expected.items()
    }:
        raise ValueError("GitHub did not retain the complete verified asset set")

    if (
        final["body"].strip()
        != (directory / "release-notes.md").read_text(encoding="utf-8").strip()
    ):
        raise ValueError("GitHub release notes differ from the reviewed notes")

    if final["draft"]:
        verify_tag(tag)
        run_release_command("edit", tag, "--draft=false", "--latest")


def verify_tag(tag: str) -> None:
    reference = read_api("git/ref/tags/" + tag)["object"]

    # Resolve annotated tags without accepting a moved tag or a different tree.
    for _ in range(5):
        if reference["type"] == "commit":
            break

        if reference["type"] != "tag":
            raise ValueError("release tag does not resolve to a commit")

        reference = read_api("git/tags/" + reference["sha"])["object"]

    if reference["type"] != "commit" or reference["sha"] != source_revision():
        raise ValueError("remote release tag differs from the tested source")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("tag", "publish"))
    parser.add_argument("directory", type=Path)
    arguments = parser.parse_args()

    if arguments.action == "tag":
        create_tag(arguments.directory)
    else:
        publish(arguments.directory)


if __name__ == "__main__":
    main()
