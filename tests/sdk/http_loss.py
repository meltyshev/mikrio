"""Lose one accepted response between owned loopback endpoints without replay."""

from __future__ import annotations

import http.client
import http.server
import os
import re
import socket
import ssl
import sys
import threading
import xml.etree.ElementTree as ET
from collections.abc import Iterator
from contextlib import contextmanager
from urllib.parse import parse_qsl, urlsplit

MAX_REQUEST_BYTES = 2 * 1024 * 1024
MAX_RESPONSE_BYTES = 64 * 1024


def accepted_response(
    status: int,
    request_id: str | None,
    body: bytes,
    *,
    expected_status: int,
    expected_root: str | None,
) -> str:
    """Reject an error or truncated final XML before calling delivery lost."""
    if status != expected_status:
        raise AssertionError("upstream did not accept the owned mutation")

    if request_id is None or re.fullmatch(r"[0-9a-f]{32}", request_id) is None:
        raise AssertionError("upstream response has no valid request identifier")

    if len(body) > MAX_RESPONSE_BYTES:
        raise AssertionError("loss response exceeded its byte bound")

    if expected_root is None:
        if body:
            raise AssertionError("unexpected body for the accepted mutation")

        return request_id

    if b"<!" in body:
        raise AssertionError("unexpected XML declaration in loss response")

    try:
        root = ET.fromstring(body)
    except ET.ParseError:
        raise AssertionError("loss response has incomplete XML") from None

    if root.tag not in {
        expected_root,
        "{http://s3.amazonaws.com/doc/2006-03-01/}" + expected_root,
    }:
        raise AssertionError("upstream response is not a completed mutation")

    return request_id


class LossServer(http.server.HTTPServer):
    """Own one bounded forwarding attempt, its observations and handler failures."""

    def __init__(self, method: str, target: str) -> None:
        endpoint = urlsplit(os.environ["mikrio_fixture_endpoint"])

        if (
            endpoint.scheme not in {"http", "https"}
            or endpoint.hostname != "127.0.0.1"
            or endpoint.port is None
            or endpoint.username is not None
            or endpoint.password is not None
            or endpoint.path not in {"", "/"}
            or endpoint.query
            or endpoint.fragment
        ):
            raise ValueError("owned loopback endpoint required")

        selected = urlsplit(target)

        if selected.scheme or selected.netloc or selected.fragment:
            raise ValueError("owned loss request target required")

        query = parse_qsl(selected.query, keep_blank_values=True)
        self.expected_status = 200
        self.expected_root = None

        if method == "PUT" and selected.path == "/photos/boto-lost-put" and not query:
            pass
        elif method == "POST" and selected.path == "/photos" and not query:
            self.expected_status = 204
        elif (
            method == "POST"
            and selected.path == "/photos/boto-lost-completion"
            and len(query) == 1
            and query[0][0] == "uploadId"
            and query[0][1]
        ):
            self.expected_root = "CompleteMultipartUploadResult"
        else:
            raise ValueError("loss request escaped the owned operation selection")

        self.method = method
        self.target = target
        self.scheme = endpoint.scheme
        self.upstream_port = endpoint.port
        self.attempts = 0
        self.observations: list[dict[str, str | int]] = []
        self.failures: list[BaseException] = []

        self.upstream_context = None
        self.server_context = None

        if self.scheme == "https":
            self.upstream_context = ssl.create_default_context(
                cafile=os.environ["mikrio_fixture_tls_ca"]
            )

            self.server_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)

            self.server_context.load_cert_chain(
                os.environ["mikrio_fixture_tls_certificate"],
                os.environ["mikrio_fixture_tls_key"],
            )

        super().__init__(("127.0.0.1", 0), LossHandler)

    def get_request(self) -> tuple[socket.socket, tuple[str, int]]:
        request, address = super().get_request()
        request.settimeout(15)

        if self.server_context is None:
            return request, address

        try:
            return self.server_context.wrap_socket(request, server_side=True), address
        except BaseException as error:
            request.close()
            self.failures.append(error)
            raise

    def handle_error(
        self,
        request: socket.socket | tuple[bytes, socket.socket],
        client_address: tuple[str, int],
    ) -> None:
        error = sys.exception()

        if error is None:
            raise RuntimeError("loss handler failed without an active exception")

        self.failures.append(error)


class LossHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    timeout = 15
    server: LossServer

    def log_message(self, format: str, *args: object) -> None:
        # Request targets, signatures, policy bodies and credentials are not logs.
        return

    def forward(self) -> None:
        self.server.attempts += 1

        if self.server.attempts != 1:
            raise AssertionError("loss fixture observed an unexpected retry")

        if self.command != self.server.method or self.path != self.server.target:
            raise AssertionError("loss request escaped its exact owned target")

        lengths = self.headers.get_all("Content-Length", [])

        if self.headers.get_all("Transfer-Encoding") or len(lengths) != 1:
            raise AssertionError("loss fixture requires one exact encoded length")

        if not lengths[0].isascii() or not lengths[0].isdecimal():
            raise AssertionError("invalid loss request length")

        length = int(lengths[0])

        if length > MAX_REQUEST_BYTES:
            raise AssertionError("loss request exceeded its byte bound")

        peer = (
            http.client.HTTPSConnection(
                "127.0.0.1",
                self.server.upstream_port,
                timeout=15,
                context=self.server.upstream_context,
            )
            if self.server.scheme == "https"
            else http.client.HTTPConnection(
                "127.0.0.1", self.server.upstream_port, timeout=15
            )
        )

        try:
            peer.putrequest(
                self.command, self.path, skip_host=True, skip_accept_encoding=True
            )

            for name, value in self.headers.items():
                if name.lower() != "connection":
                    peer.putheader(name, value)

            peer.putheader("Connection", "close")
            peer.endheaders()
            remaining = length

            while remaining:
                chunk = self.rfile.read(min(remaining, 64 * 1024))

                if not chunk:
                    raise EOFError("loss request source ended early")

                peer.send(chunk)
                remaining -= len(chunk)

            with peer.getresponse() as response:
                request_id = accepted_response(
                    response.status,
                    response.getheader("x-amz-request-id"),
                    response.read(MAX_RESPONSE_BYTES + 1),
                    expected_status=self.server.expected_status,
                    expected_root=self.server.expected_root,
                )

                self.server.observations.append(
                    {
                        "method": self.command,
                        "status": response.status,
                        "encoded_bytes": length,
                        "request_id": request_id,
                    }
                )

            # The complete positive upstream response never reaches the client.
            self.close_connection = True
        finally:
            peer.close()

    do_PUT = forward
    do_POST = forward


@contextmanager
def lost_response(method: str, target: str) -> Iterator[tuple[str, LossServer]]:
    """Match upstream transport, then join and report every owned handler failure."""
    with LossServer(method, target) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=False)
        thread.start()

        try:
            yield f"{server.scheme}://127.0.0.1:{server.server_port}", server
        finally:
            primary = sys.exception()
            server.shutdown()
            thread.join(timeout=20)

            if thread.is_alive():
                server.failures.append(RuntimeError("owned loss server did not settle"))

            if server.failures:
                failures = server.failures.copy()

                if primary is not None:
                    failures.insert(0, primary)

                raise BaseExceptionGroup("owned response loss failed", failures)
