"""GitHub publication requires the tested tag and matching immutable assets."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from tools import publish_github
from tools.artifacts import digest


class GithubPublishTests(unittest.TestCase):
    def test_draft_lookup_includes_later_pages(self) -> None:
        draft = {"tag_name": "v0.1.0", "draft": True, "id": 42}
        pages = [[{"tag_name": "v0.0.9", "draft": False}], [draft]]

        with patch(
            "tools.publish_github.subprocess.check_output",
            return_value=json.dumps(pages),
        ):
            self.assertEqual(publish_github.find_release("v0.1.0"), draft)
            self.assertIsNone(publish_github.find_release("v0.2.0"))

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
