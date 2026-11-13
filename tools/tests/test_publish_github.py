"""GitHub publication requires the tested tag and matching immutable assets."""

from __future__ import annotations

import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from tools import publish_github
from tools.artifacts import digest
from tools.release import REPOSITORY


class GithubPublishTests(unittest.TestCase):
    def test_tag_creation_uses_the_verified_commit_and_exact_tag_name(self) -> None:
        with (
            patch.object(publish_github, "verify") as verify,
            patch.object(publish_github, "check", return_value="0.1.0"),
            patch.object(publish_github, "source_revision", return_value="reviewed"),
            patch(
                "tools.publish_github.subprocess.check_output",
                return_value=json.dumps([{"ref": "refs/tags/v0.1.00"}]),
            ),
            patch.object(
                publish_github,
                "read_api",
                return_value={"object": {"type": "commit", "sha": "reviewed"}},
            ),
            patch("tools.publish_github.subprocess.run") as run,
        ):
            directory = Path("release")

            publish_github.create_tag(directory)

            verify.assert_called_once_with(directory)

            run.assert_called_once_with(
                [
                    "gh",
                    "api",
                    f"repos/{REPOSITORY}/git/refs",
                    "--method",
                    "POST",
                    "--raw-field",
                    "ref=refs/tags/v0.1.0",
                    "--raw-field",
                    "sha=reviewed",
                ],
                check=True,
            )

    def test_tag_retry_accepts_matching_lightweight_and_annotated_tags(self) -> None:
        for objects in (
            [{"type": "commit", "sha": "reviewed"}],
            [{"type": "tag", "sha": "tagged"}, {"type": "commit", "sha": "reviewed"}],
        ):
            with (
                self.subTest(objects=objects),
                patch.object(publish_github, "verify"),
                patch.object(publish_github, "check", return_value="0.1.0"),
                patch.object(
                    publish_github, "source_revision", return_value="reviewed"
                ),
                patch(
                    "tools.publish_github.subprocess.check_output",
                    return_value=json.dumps([{"ref": "refs/tags/v0.1.0"}]),
                ),
                patch.object(
                    publish_github,
                    "read_api",
                    side_effect=[{"object": item} for item in objects],
                ),
                patch("tools.publish_github.subprocess.run") as run,
            ):
                publish_github.create_tag(Path("release"))

                run.assert_not_called()

    def test_tag_creation_never_overwrites_a_different_commit(self) -> None:
        with (
            patch.object(publish_github, "verify"),
            patch.object(publish_github, "check", return_value="0.1.0"),
            patch.object(publish_github, "source_revision", return_value="reviewed"),
            patch(
                "tools.publish_github.subprocess.check_output",
                return_value=json.dumps([{"ref": "refs/tags/v0.1.0"}]),
            ),
            patch.object(
                publish_github,
                "read_api",
                return_value={"object": {"type": "commit", "sha": "different"}},
            ),
            patch("tools.publish_github.subprocess.run") as run,
        ):
            with self.assertRaisesRegex(ValueError, "tested source"):
                publish_github.create_tag(Path("release"))

            run.assert_not_called()

    def test_tag_retry_recovers_after_a_committed_request_loses_its_response(
        self,
    ) -> None:
        reference = {"ref": "refs/tags/v0.1.0"}

        with (
            patch.object(publish_github, "verify"),
            patch.object(publish_github, "check", return_value="0.1.0"),
            patch.object(publish_github, "source_revision", return_value="reviewed"),
            patch(
                "tools.publish_github.subprocess.check_output",
                side_effect=["[]", json.dumps([reference])],
            ),
            patch.object(
                publish_github,
                "read_api",
                return_value={"object": {"type": "commit", "sha": "reviewed"}},
            ),
            patch(
                "tools.publish_github.subprocess.run",
                side_effect=subprocess.CalledProcessError(1, "gh"),
            ) as run,
        ):
            directory = Path("release")

            with self.assertRaises(subprocess.CalledProcessError):
                publish_github.create_tag(directory)

            publish_github.create_tag(directory)

            run.assert_called_once()

    def test_invalid_release_artifacts_cannot_create_a_tag(self) -> None:
        with (
            patch.object(
                publish_github, "verify", side_effect=ValueError("checksum mismatch")
            ),
            patch("tools.publish_github.subprocess.run") as run,
            patch("tools.publish_github.subprocess.check_output") as read,
        ):
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                publish_github.create_tag(Path("release"))

            run.assert_not_called()
            read.assert_not_called()

    def test_lookup_distinguishes_missing_draft_and_published_releases(self) -> None:
        for release in (
            None,
            {"tag_name": "v0.1.0", "draft": True, "id": 42},
            {"tag_name": "v0.1.0", "draft": False, "id": 42},
        ):
            reference = None if release is None else {"databaseId": 42}
            response = {"data": {"repository": {"release": reference}}}

            with (
                self.subTest(release=release),
                patch(
                    "tools.publish_github.subprocess.check_output",
                    side_effect=[json.dumps(response), json.dumps(release)],
                ),
            ):
                self.assertEqual(publish_github.find_release("v0.1.0"), release)

    def test_lookup_errors_cannot_create_a_release(self) -> None:
        response = json.dumps({"data": {"repository": {"release": {"databaseId": 42}}}})

        for stage in ("graphql", "rest"):
            error = subprocess.CalledProcessError(1, "gh api")
            responses = [error] if stage == "graphql" else [response, error]

            with (
                self.subTest(stage=stage),
                patch.object(publish_github, "verify"),
                patch.object(publish_github, "check", return_value="0.1.0"),
                patch.object(publish_github, "verify_tag"),
                patch(
                    "tools.publish_github.subprocess.check_output",
                    side_effect=responses,
                ),
                patch("tools.publish_github.subprocess.run") as run,
            ):
                with self.assertRaises(subprocess.CalledProcessError):
                    publish_github.publish(Path("release"))

                run.assert_not_called()

    def test_lookup_rejects_a_release_for_a_different_tag(self) -> None:
        response = {"data": {"repository": {"release": {"databaseId": 42}}}}
        release = {"tag_name": "v0.1.00", "draft": True, "id": 42}

        with patch(
            "tools.publish_github.subprocess.check_output",
            side_effect=[json.dumps(response), json.dumps(release)],
        ):
            with self.assertRaisesRegex(ValueError, "requested tag"):
                publish_github.find_release("v0.1.0")

    def test_new_draft_is_published_while_release_listings_lag(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / "archive.tgz").write_bytes(b"reviewed archive")

            (directory / "release-notes.md").write_text(
                "Reviewed notes\n", encoding="utf-8", newline="\n"
            )

            release: dict[str, Any] = {
                "id": 42,
                "tag_name": "v0.1.0",
                "draft": True,
                "assets": [],
                "body": "Reviewed notes",
            }

            created = False
            mutations: list[str] = []

            def read(command: list[str], *, encoding: str) -> str:
                self.assertEqual(command[:2], ["gh", "api"])
                endpoint = command[2]

                if endpoint == "graphql":
                    self.assertIn("owner=meltyshev", command)
                    self.assertIn("name=mikrio", command)
                    self.assertIn("tag=v0.1.0", command)
                    reference = {"databaseId": 42} if created else None

                    return json.dumps({"data": {"repository": {"release": reference}}})

                if endpoint == f"repos/{REPOSITORY}/releases?per_page=100":
                    return "[[]]"

                if endpoint == f"repos/{REPOSITORY}/git/ref/tags/v0.1.0":
                    return json.dumps({"object": {"type": "commit", "sha": "reviewed"}})

                if endpoint == f"repos/{REPOSITORY}/releases/42" and created:
                    return json.dumps(release)

                raise AssertionError(f"unexpected API request: {command}")

            def run(command: list[str], *, check: bool) -> None:
                nonlocal created
                self.assertEqual(command[:2], ["gh", "release"])
                self.assertEqual(command[3], "v0.1.0")
                self.assertEqual(command[-2:], ["--repo", REPOSITORY])
                action = command[2]

                if action == "create":
                    self.assertFalse(created)
                    self.assertIn("--draft", command)
                    created = True
                elif action == "upload":
                    self.assertTrue(created)
                    self.assertTrue(release["draft"])
                    path = Path(command[4])

                    release["assets"].append(
                        {
                            "name": path.name,
                            "state": "uploaded",
                            "digest": "sha256:" + digest(path),
                        }
                    )
                elif action == "edit":
                    self.assertEqual(
                        {asset["name"] for asset in release["assets"]},
                        {"archive.tgz", "release-notes.md"},
                    )

                    self.assertIn("--draft=false", command)
                    release["draft"] = False
                else:
                    raise AssertionError(f"unexpected release command: {command}")

                mutations.append(action)

            with (
                patch.object(publish_github, "verify"),
                patch.object(publish_github, "check", return_value="0.1.0"),
                patch.object(
                    publish_github, "source_revision", return_value="reviewed"
                ),
                patch("tools.publish_github.subprocess.check_output", side_effect=read),
                patch("tools.publish_github.subprocess.run", side_effect=run),
            ):
                publish_github.publish(directory)
                publish_github.publish(directory)

            self.assertFalse(release["draft"])
            self.assertEqual(mutations, ["create", "upload", "upload", "edit"])

    def test_resuming_a_draft_uploads_only_missing_assets_before_publication(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / "archive.tgz").write_bytes(b"reviewed archive")

            (directory / "release-notes.md").write_text(
                "Reviewed notes\n", encoding="utf-8", newline="\n"
            )

            assets = [
                {
                    "name": path.name,
                    "state": "uploaded",
                    "digest": "sha256:" + digest(path),
                }
                for path in sorted(directory.iterdir())
            ]

            draft = {
                "id": 42,
                "draft": True,
                "assets": assets[:1],
                "body": "Reviewed notes",
            }

            final = draft | {"assets": assets}

            with (
                patch.object(publish_github, "verify"),
                patch.object(publish_github, "check", return_value="0.1.0"),
                patch.object(publish_github, "verify_tag"),
                patch.object(publish_github, "find_release", return_value=draft),
                patch.object(publish_github, "read_api", return_value=final) as read,
                patch.object(publish_github, "run_release_command") as command,
            ):
                publish_github.publish(directory)

            read.assert_called_once_with("releases/42")

            self.assertEqual(
                command.call_args_list,
                [
                    unittest.mock.call(
                        "upload", "v0.1.0", str(directory / "release-notes.md")
                    ),
                    unittest.mock.call("edit", "v0.1.0", "--draft=false", "--latest"),
                ],
            )

    def test_moved_tag_cannot_create_or_publish_a_release(self) -> None:
        with (
            patch.object(publish_github, "verify"),
            patch.object(publish_github, "source_revision", return_value="reviewed"),
            patch.object(
                publish_github,
                "read_api",
                return_value={"object": {"type": "commit", "sha": "different"}},
            ),
            patch.object(publish_github, "run_release_command") as run,
        ):
            with self.assertRaisesRegex(ValueError, "tested source"):
                publish_github.publish(Path("unused"))

            run.assert_not_called()

    def test_existing_asset_mismatch_cannot_be_overwritten(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / "archive.tgz").write_bytes(b"reviewed")

            draft = {
                "draft": True,
                "assets": [
                    {
                        "name": "archive.tgz",
                        "state": "uploaded",
                        "digest": "sha256:wrong",
                    }
                ],
            }

            with (
                patch.object(publish_github, "verify"),
                patch.object(
                    publish_github, "source_revision", return_value="reviewed"
                ),
                patch.object(publish_github, "verify_tag"),
                patch.object(publish_github, "find_release", return_value=draft),
                patch.object(publish_github, "run_release_command") as run,
            ):
                with self.assertRaisesRegex(ValueError, "different bytes"):
                    publish_github.publish(directory)

                run.assert_not_called()
