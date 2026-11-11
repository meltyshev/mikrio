"""Controlled loopback checks for forwarding, final-response loss and refusal."""

from __future__ import annotations

import http.client
import http.server
import os
import threading
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from http_loss import MAX_REQUEST_BYTES, lost_response

REQUEST_ID = "0123456789abcdef0123456789abcdef"
TARGET = "/photos/boto-lost-put"


class Upstream(http.server.HTTPServer):
    def __init__(self, status: int) -> None:
        self.status = status
        self.requests: list[tuple[str, str, str, bytes]] = []
        super().__init__(("127.0.0.1", 0), UpstreamHandler)


class UpstreamHandler(http.server.BaseHTTPRequestHandler):
    server: Upstream
    timeout = 3

    def log_message(self, format: str, *args: object) -> None:
        return

    def do_PUT(self) -> None:
        body = self.rfile.read(int(self.headers["Content-Length"]))

        self.server.requests.append(
            (
                self.path,
                self.headers["Host"],
                self.headers["Authorization"],
                body,
            )
        )

        self.send_response(self.server.status)
        self.send_header("x-amz-request-id", REQUEST_ID)
        self.send_header("Content-Length", "0")
        self.end_headers()


class ResponseLossTests(unittest.TestCase):
    def setUp(self) -> None:
        self.upstream = Upstream(200)
        self.addCleanup(self.upstream.server_close)
        self.thread = threading.Thread(target=self.upstream.serve_forever, daemon=False)
        self.thread.start()
        self.addCleanup(self.stop_upstream)

        environment = patch.dict(
            os.environ,
            {
                "mikrio_fixture_endpoint": f"http://127.0.0.1:{self.upstream.server_port}"
            },
        )

        environment.start()
        self.addCleanup(environment.stop)

    def stop_upstream(self) -> None:
        self.upstream.shutdown()
        self.thread.join(timeout=5)
        self.assertFalse(self.thread.is_alive())

    def exchange(self, endpoint: str, *, oversized: bool = False) -> None:
        port = urlsplit(endpoint).port
        self.assertIsNotNone(port)
        client = http.client.HTTPConnection("127.0.0.1", port, timeout=3)

        try:
            headers = {"Authorization": "synthetic-owned-authorization"}

            if oversized:
                client.putrequest("PUT", TARGET)
                client.putheader("Content-Length", str(MAX_REQUEST_BYTES + 1))
                client.endheaders()
            else:
                client.request("PUT", TARGET, body=b"owned payload", headers=headers)

            with self.assertRaises(http.client.RemoteDisconnected):
                client.getresponse()
        finally:
            client.close()

    def test_complete_acceptance_is_forwarded_once_and_never_delivered(self) -> None:
        with lost_response("PUT", TARGET) as (endpoint, server):
            self.exchange(endpoint)
            self.assertEqual(server.attempts, 1)

            self.assertEqual(
                server.observations,
                [
                    {
                        "method": "PUT",
                        "status": 200,
                        "encoded_bytes": 13,
                        "request_id": REQUEST_ID,
                    }
                ],
            )

            self.assertEqual(
                self.upstream.requests,
                [
                    (
                        TARGET,
                        urlsplit(endpoint).netloc,
                        "synthetic-owned-authorization",
                        b"owned payload",
                    )
                ],
            )

    def test_upstream_failure_is_reported_and_never_recorded_as_accepted(self) -> None:
        self.upstream.status = 403

        with self.assertRaises(ExceptionGroup) as raised:
            with lost_response("PUT", TARGET) as (endpoint, server):
                self.exchange(endpoint)
                self.assertEqual(server.observations, [])

        self.assertEqual(len(raised.exception.exceptions), 1)
        self.assertIsInstance(raised.exception.exceptions[0], AssertionError)
        self.assertEqual(len(self.upstream.requests), 1)

    def test_oversized_request_is_refused_before_opening_the_upstream(self) -> None:
        with self.assertRaises(ExceptionGroup) as raised:
            with lost_response("PUT", TARGET) as (endpoint, server):
                self.exchange(endpoint, oversized=True)
                self.assertEqual(server.observations, [])

        self.assertEqual(len(raised.exception.exceptions), 1)
        self.assertIsInstance(raised.exception.exceptions[0], AssertionError)
        self.assertEqual(self.upstream.requests, [])


if __name__ == "__main__":
    unittest.main()
