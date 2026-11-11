"""Independent signed wire cases for the actual native fixture and nginx."""

import base64
import concurrent.futures
import hashlib
import http.client
import io
import os
import socket
import ssl
import threading
import time
import unittest
import zlib
from urllib.parse import urlsplit

from http_sigv4 import integrity, request


class RawTests(unittest.TestCase):
    def setUp(self) -> None:
        endpoint = urlsplit(os.environ["mikrio_fixture_endpoint"])
        self.assertIn(endpoint.scheme, {"http", "https"})
        self.assertEqual(endpoint.hostname, "127.0.0.1")
        assert endpoint.hostname is not None and endpoint.port is not None
        self.address = (endpoint.hostname, endpoint.port)

        self.tls_context = (
            ssl.create_default_context(cafile=os.environ["mikrio_fixture_tls_ca"])
            if endpoint.scheme == "https"
            else None
        )

    def connection(self, timeout_seconds: float = 5) -> socket.socket:
        connection = socket.create_connection(self.address, timeout=timeout_seconds)

        if self.tls_context is None:
            return connection

        try:
            return self.tls_context.wrap_socket(
                connection, server_hostname=self.address[0]
            )
        except BaseException:
            connection.close()
            raise

    def exchange(self, raw: bytes) -> bytes:
        with self.connection() as connection:
            connection.sendall(raw)
            output = bytearray()

            while chunk := connection.recv(4096):
                output.extend(chunk)
                self.assertLessEqual(len(output), 64 * 1024)

        return bytes(output)

    def get(self, path: str, host: str = "localhost:9000") -> bytes:
        return request(
            "GET",
            path,
            "header",
            "mikrio_a00112233445566778899AABBCCDDEEFF",
            "synthetic-http-probe-secret",
            "us-east-1",
            "s3",
            b"",
            {"host": host},
        )

    def test_signed_path_and_host_reach_the_same_exact_key(self) -> None:
        cases = [
            ("a", b"a"),
            ("a/b", b"a/b"),
            ("a//b/", b"a//b/"),
            ("a/./b", b"a/./b"),
            ("a/../b", b"a/../b"),
            ("a%2Fb", b"a/b"),
            ("a+b", b"a+b"),
            ("a%2Bb", b"a+b"),
            ("a%20b", b"a b"),
            ("a%252Fb", b"a%2Fb"),
            ("%C3%A9", "é".encode()),
            ("e%CC%81", "e\u0301".encode()),
        ]

        for path, expected in cases:
            with self.subTest(path=path):
                raw = self.exchange(self.get("/photos/" + path))

                # HTTPResponse uses makefile; its stub requires a concrete socket.
                with http.client.HTTPResponse(ResponseSocket(raw)) as response:  # type: ignore[arg-type]
                    response.begin()
                    self.assertEqual(response.status, 200)
                    self.assertEqual(response.read(), expected)

        signed = self.get("/photos/host", f"127.0.0.1:{self.address[1]}")
        self.assertTrue(self.exchange(signed).startswith(b"HTTP/1.1 200"))
        tampered = signed.replace(b"host: 127.0.0.1:", b"host: localhost:")
        self.assertTrue(self.exchange(tampered).startswith(b"HTTP/1.1 403"))

    def test_header_and_query_authentication_bind_methods_and_credential_scope(
        self,
    ) -> None:
        key = "mikrio_a00112233445566778899AABBCCDDEEFF"
        secret = "synthetic-http-probe-secret"

        for mode in ("header", "query"):
            valid = request(
                "GET",
                "/photos/dispatch-probe-control",
                mode,
                key,
                secret,
                "us-east-1",
                "s3",
                b"",
            )

            self.assertTrue(self.exchange(valid).startswith(b"HTTP/1.1 200"), mode)

            for method in ("GET", "HEAD", "PUT", "DELETE"):
                signed_method = "HEAD" if method == "GET" else "GET"

                signed = request(
                    signed_method,
                    "/photos/dispatch-probe-method",
                    mode,
                    key,
                    secret,
                    "us-east-1",
                    "s3",
                    b"",
                )

                changed = method.encode() + b" " + signed.split(b" ", 1)[1]

                self.assertTrue(
                    self.exchange(changed).startswith(b"HTTP/1.1 403"), (mode, method)
                )

                for region, service, status_code in (
                    ("eu-west-1", "s3", 400),
                    ("us-east-1", "sts", 501),
                ):
                    scoped = request(
                        method,
                        "/photos/dispatch-probe-scope",
                        mode,
                        key,
                        secret,
                        region,
                        service,
                        b"",
                    )

                    self.assertTrue(
                        self.exchange(scoped).startswith(
                            f"HTTP/1.1 {status_code}".encode()
                        ),
                        (mode, method, region, service),
                    )

        for method in ("GET", "HEAD", "PUT", "DELETE"):
            expired = request(
                method,
                "/photos/dispatch-probe-expired",
                "expired",
                key,
                secret,
                "us-east-1",
                "s3",
                b"",
            )

            self.assertTrue(self.exchange(expired).startswith(b"HTTP/1.1 403"), method)

    def test_ambiguous_framing_closes_before_a_pipelined_request(self) -> None:
        valid = self.get("/photos/dispatch-probe-pipeline-next")

        candidates = [
            b"host: duplicate\r\n",
            b"authorization: invalid\r\n",
            b"content-length: 1\r\n",
            b"transfer-encoding: chunked\r\n",
        ]

        for extra in candidates:
            with self.subTest(field=extra.split(b":")[0]):
                first = self.get("/photos/dispatch-probe-pipeline-first").replace(
                    b"connection: close\r\n", extra + b"connection: keep-alive\r\n"
                )

                response = self.exchange(first + valid)
                self.assertTrue(response.startswith(b"HTTP/1.1 400"))
                self.assertEqual(response.count(b"HTTP/1.1 "), 1)
                self.assertIn(b"connection: close", response.lower())

    def test_stalled_header_and_body_close_and_a_fresh_request_still_succeeds(
        self,
    ) -> None:
        body = request(
            "PUT",
            "/photos/stalled",
            "header",
            "mikrio_a00112233445566778899AABBCCDDEEFF",
            "synthetic-http-probe-secret",
            "us-east-1",
            "s3",
            b"123456789",
            {"x-amz-content-sha256": "UNSIGNED-PAYLOAD"},
        )[:-6]

        for phase, unfinished in (
            ("header", b"GET /photos/stalled HTTP/1.1\r\nhost: localhost:9000\r\n"),
            ("body", body),
        ):
            with self.subTest(phase=phase), self.connection(40) as connection:
                started = time.monotonic()
                connection.sendall(unfinished)
                output = bytearray()

                try:
                    while chunk := connection.recv(4096):
                        output.extend(chunk)
                        self.assertLessEqual(len(output), 64 * 1024)
                except ConnectionResetError:
                    pass

                elapsed = time.monotonic() - started
                self.assertLess(elapsed, 40)
                self.assertNotIn(b"HTTP/1.1 200", output)

                if output:
                    self.assertTrue(output.startswith((b"HTTP/1.1 4", b"HTTP/1.1 5")))

                print(
                    f"STALLED_PEER phase={phase} elapsed_seconds={elapsed:.6f} response_bytes={len(output)}",
                    flush=True,
                )

            healthy = self.exchange(self.get("/photos/after-stall"))

            with http.client.HTTPResponse(ResponseSocket(healthy)) as response:  # type: ignore[arg-type]
                response.begin()
                self.assertEqual(response.status, 200)
                self.assertEqual(response.read(), b"after-stall")

    def test_healthy_progress_continues_beyond_the_stalled_peer_budget(self) -> None:
        chunk = b"p" * 4096
        chunk_count = 35
        payload = chunk * chunk_count
        checksum = base64.b64encode(zlib.crc32(payload).to_bytes(4, "big")).decode()

        raw = request(
            "PUT",
            "/photos/healthy-progress",
            "header",
            "mikrio_a00112233445566778899AABBCCDDEEFF",
            "synthetic-http-probe-secret",
            "us-east-1",
            "s3",
            b"",
            {
                "x-amz-content-sha256": "UNSIGNED-PAYLOAD",
                "x-amz-checksum-crc32": checksum,
            },
        ).replace(
            b"content-length: 0\r\n", f"content-length: {len(payload)}\r\n".encode()
        )

        with self.connection(10) as connection:
            connection.sendall(raw)
            started = time.monotonic()

            for index in range(chunk_count):
                if index:
                    threading.Event().wait(1)

                connection.sendall(chunk)

            transfer_seconds = time.monotonic() - started
            self.assertGreaterEqual(transfer_seconds, 33)

            with http.client.HTTPResponse(connection) as response:
                response.begin()
                self.assertEqual(response.status, 200)

                self.assertEqual(
                    response.getheader("ETag"), f'"{hashlib.md5(payload).hexdigest()}"'
                )

                self.assertEqual(response.getheader("x-amz-checksum-crc32"), checksum)
                self.assertEqual(response.read(), b"")

        print(
            f"HEALTHY_PROGRESS bytes={len(payload)} chunks={chunk_count} transfer_seconds={transfer_seconds:.6f}",
            flush=True,
        )

    def test_invalid_crc32_framing_never_finishes(self) -> None:
        for mode in [
            "bad-header",
            "bad-md5",
            "bad-trailer",
            "missing-trailer",
            "duplicate-trailer",
            "extra-decoded",
            "short-decoded",
        ]:
            with self.subTest(mode=mode):
                self.assertTrue(
                    self.exchange(integrity(mode)).startswith(b"HTTP/1.1 4")
                )

    def test_large_reused_chunk_stream_and_abandoned_body_have_exact_finishes(
        self,
    ) -> None:
        chunk = b"x" * (16 * 1024)
        chunk_count = 1024
        md5 = hashlib.md5()
        crc32 = 0

        for _ in range(chunk_count):
            md5.update(chunk)
            crc32 = zlib.crc32(chunk, crc32)

        headers = {
            "x-amz-content-sha256": "UNSIGNED-PAYLOAD",
            "x-amz-checksum-crc32": base64.b64encode(crc32.to_bytes(4, "big")).decode(),
        }

        raw = request(
            "PUT",
            "/photos/large-stream",
            "header",
            "mikrio_a00112233445566778899AABBCCDDEEFF",
            "synthetic-http-probe-secret",
            "us-east-1",
            "s3",
            b"",
            headers,
        ).replace(b"content-length: 0\r\n", b"content-length: 16777216\r\n")

        with self.connection() as connection:
            connection.sendall(raw)

            for _ in range(chunk_count):
                connection.sendall(chunk)

            with http.client.HTTPResponse(connection) as response:
                response.begin()
                self.assertEqual(response.status, 200)
                self.assertEqual(response.getheader("ETag"), f'"{md5.hexdigest()}"')

                self.assertEqual(
                    response.getheader("x-amz-checksum-crc32"),
                    headers["x-amz-checksum-crc32"],
                )

                self.assertEqual(response.read(), b"")

        with self.connection() as connection:
            connection.sendall(raw)
            connection.sendall(chunk)

            if self.tls_context is not None:
                # Close the TLS peer before its declared file is complete. The
                # independently counted native finishes must remain unchanged.
                return

            connection.shutdown(socket.SHUT_WR)
            response = http.client.HTTPResponse(connection)

            try:
                response.begin()
                self.assertGreaterEqual(response.status, 400)
                self.assertLess(response.status, 500)
                response.read()
            except (ConnectionResetError, http.client.RemoteDisconnected):
                # Closing/resetting an incomplete request is allowed. The
                # runner's exact finish/byte totals prove it did not finish.
                pass
            finally:
                response.close()

    def test_concurrent_reused_chunk_streams_preserve_bytes_at_two_sizes(self) -> None:
        for payload_bytes in [1024 * 1024, 64 * 1024 * 1024]:
            with self.subTest(size=payload_bytes):
                self.concurrent_streams(payload_bytes)

    def concurrent_streams(self, payload_bytes: int) -> None:
        chunk = b"x" * (16 * 1024)
        md5 = hashlib.md5()
        crc32 = 0

        for _ in range(payload_bytes // len(chunk)):
            md5.update(chunk)
            crc32 = zlib.crc32(chunk, crc32)

        checksum = base64.b64encode(crc32.to_bytes(4, "big")).decode()
        ready = threading.Barrier(5, timeout=10)
        resume = threading.Event()

        def upload(index: int) -> None:
            raw = request(
                "PUT",
                f"/photos/concurrent-{payload_bytes}-{index}",
                "header",
                "mikrio_a00112233445566778899AABBCCDDEEFF",
                "synthetic-http-probe-secret",
                "us-east-1",
                "s3",
                b"",
                {
                    "x-amz-content-sha256": "UNSIGNED-PAYLOAD",
                    "x-amz-checksum-crc32": checksum,
                },
            ).replace(
                b"content-length: 0\r\n",
                f"content-length: {payload_bytes}\r\n".encode(),
            )

            with self.connection(10) as connection:
                connection.sendall(raw)
                connection.sendall(chunk)
                ready.wait()

                if not resume.wait(10):
                    raise TimeoutError("concurrent upload release was not observed")

                for _ in range(payload_bytes // len(chunk) - 1):
                    connection.sendall(chunk)

                with http.client.HTTPResponse(connection) as response:
                    response.begin()
                    self.assertEqual(response.status, 200)
                    self.assertEqual(response.getheader("ETag"), f'"{md5.hexdigest()}"')

                    self.assertEqual(
                        response.getheader("x-amz-checksum-crc32"), checksum
                    )

                    self.assertEqual(response.read(), b"")

        started = time.monotonic()

        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
            pending = [executor.submit(upload, index) for index in range(4)]

            try:
                ready.wait()
                # Hold all producers for one sampling window. The native
                # fixture independently records actual overlapping work.
                threading.Event().wait(1)
            finally:
                resume.set()

            for future in pending:
                future.result(timeout=30)

        print(
            f"CONCURRENT_STREAM bytes_per_upload={payload_bytes} concurrency=4 "
            f"started_monotonic_seconds={started:.6f} "
            f"elapsed_seconds={time.monotonic() - started:.6f} "
            "producer_hold_seconds=1 reused_chunk_bytes=16384",
            flush=True,
        )


class ResponseSocket:
    """Supply captured bytes to Python's standard HTTP response decoder."""

    def __init__(self, raw: bytes) -> None:
        self.raw = raw

    def makefile(self, _mode: str) -> io.BytesIO:
        return io.BytesIO(self.raw)
