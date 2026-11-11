"""Reject incomplete or failed responses in the lost-response test fixture."""

from __future__ import annotations

import unittest

from http_loss import MAX_RESPONSE_BYTES, accepted_response


class ResponseLossFixtureTests(unittest.TestCase):
    def test_only_complete_success_responses_qualify_as_lost_acknowledgments(
        self,
    ) -> None:
        request_id = "0123456789abcdef0123456789abcdef"

        for status, body, root in [
            (200, b"", None),
            (204, b"", None),
            (
                200,
                b'<CompleteMultipartUploadResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><ETag>complete</ETag></CompleteMultipartUploadResult>',
                "CompleteMultipartUploadResult",
            ),
        ]:
            with self.subTest(status=status, root=root):
                self.assertEqual(
                    accepted_response(
                        status,
                        request_id,
                        body,
                        expected_status=status,
                        expected_root=root,
                    ),
                    request_id,
                )

    def test_status_xml_identity_and_byte_bounds_cannot_be_counted_as_success(
        self,
    ) -> None:
        request_id = "0123456789abcdef0123456789abcdef"

        for status, identity, body, root in [
            (403, request_id, b"", None),
            (200, None, b"", None),
            (200, "caller-supplied", b"", None),
            (200, request_id, b"unexpected", None),
            (200, request_id, b"x" * (MAX_RESPONSE_BYTES + 1), None),
            (
                200,
                request_id,
                b"<Error><Code>InternalError</Code></Error>",
                "CompleteMultipartUploadResult",
            ),
            (
                200,
                request_id,
                b"<CompleteMultipartUploadResult>",
                "CompleteMultipartUploadResult",
            ),
            (
                200,
                request_id,
                b'<!DOCTYPE x [<!ENTITY e SYSTEM "file:///private/canary">]><CompleteMultipartUploadResult/>',
                "CompleteMultipartUploadResult",
            ),
        ]:
            with (
                self.subTest(status=status, root=root, body_bytes=len(body)),
                self.assertRaises(AssertionError),
            ):
                accepted_response(
                    status,
                    identity,
                    body,
                    expected_status=200,
                    expected_root=root,
                )


if __name__ == "__main__":
    unittest.main()
