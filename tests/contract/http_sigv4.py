"""Independent, stdlib-only raw request producer for the private HTTP probe.

Origin: AWS-document-derived, not an AWS capture or SDK interoperability proof.
Sources (read 2026-10-08):
https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sig-v4-header-based-auth.html
https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sigv4-query-string-auth.html
Uses only explicit synthetic credentials; no environment credential lookup.
"""

import base64
import hashlib
import hmac
import json
import sys
import zlib
from datetime import UTC, datetime, timedelta
from pathlib import Path
from urllib.parse import quote, unquote_to_bytes

PROBE_ACCESS_KEY_ID = "mikrio_a00112233445566778899AABBCCDDEEFF"
PROBE_SECRET_ACCESS_KEY = "synthetic-http-probe-secret"


def sign_request(
    canonical: str, date: str, region: str, service: str, secret: str
) -> tuple[str, str]:
    """Return the credential scope and signature for a canonical request."""
    scope = f"{date[:8]}/{region}/{service}/aws4_request"
    canonical_hash = hashlib.sha256(canonical.encode()).hexdigest()
    string_to_sign = f"AWS4-HMAC-SHA256\n{date}\n{scope}\n{canonical_hash}"
    signing_key = ("AWS4" + secret).encode()

    for scope_part in (date[:8], region, service, "aws4_request"):
        signing_key = hmac.digest(signing_key, scope_part.encode(), "sha256")

    return scope, hmac.new(signing_key, string_to_sign.encode(), "sha256").hexdigest()


def sign_chunk(payload: bytes, previous: str, date: str, secret: str) -> str:
    """Sign one probe chunk using the preceding request or chunk signature."""
    scope = f"{date[:8]}/us-east-1/s3/aws4_request"
    signing_key = ("AWS4" + secret).encode()

    for scope_part in (date[:8], "us-east-1", "s3", "aws4_request"):
        signing_key = hmac.digest(signing_key, scope_part.encode(), "sha256")

    string_to_sign = (
        f"AWS4-HMAC-SHA256-PAYLOAD\n{date}\n{scope}\n{previous}\n"
        f"{hashlib.sha256(b'').hexdigest()}\n{hashlib.sha256(payload).hexdigest()}"
    )

    return hmac.new(signing_key, string_to_sign.encode(), "sha256").hexdigest()


def check_known_vectors() -> None:
    """Assert request and chunk signatures against AWS's published examples."""
    empty_hash = hashlib.sha256(b"").hexdigest()

    canonical = (
        "GET\n/test.txt\n\nhost:examplebucket.s3.amazonaws.com\nrange:bytes=0-9\n"
        f"x-amz-content-sha256:{empty_hash}\nx-amz-date:20130524T000000Z\n\n"
        f"host;range;x-amz-content-sha256;x-amz-date\n{empty_hash}"
    )

    _, actual_signature = sign_request(
        canonical,
        "20130524T000000Z",
        "us-east-1",
        "s3",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )

    assert (
        actual_signature
        == "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    )

    # AWS's published streaming-trailers example supplies an independent chunk.
    # https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sigv4-streaming-trailers.html
    assert (
        sign_chunk(
            b"a" * 65536,
            "106e2a8a18243abcf37539882f36619c00e2dfc72633413f02d3b74544bfeb8e",
            "20130524T000000Z",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        )
        == "b474d8862b1487a5145d686f57f013e54db672cee1c953b3010fb58501ef5aa2"
    )


def request(
    method: str,
    path: str,
    mode: str,
    key: str,
    secret: str,
    region: str,
    service: str,
    payload: bytes,
    extra_headers: dict[str, str] | None = None,
    query_fields: dict[str, str] | None = None,
) -> bytes:
    """Build a raw signed probe request, including deliberately invalid modes.

    Supply an encoded path separately from query fields and lowercase header
    names. The key and secret are explicit test credentials; no request is sent.
    Query modes declare only Host as signed; additional headers form negative probes.
    """
    check_known_vectors()
    date = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")

    if mode == "expired":
        date = "20130524T000000Z"

    canonical_path = quote(unquote_to_bytes(path), safe="/~")

    if mode in ("raw-path", "raw-path-query"):
        canonical_path = path  # Deliberately invalid alternative interpretation.

    headers = {"host": "localhost:9000"}
    body_hash = hashlib.sha256(payload).hexdigest()
    fields = dict(query_fields or {})

    if mode in ("query", "expired", "raw-path-query"):
        scope = f"{date[:8]}/{region}/{service}/aws4_request"

        fields.update(
            {
                "X-Amz-Algorithm": "AWS4-HMAC-SHA256",
                "X-Amz-Credential": f"{key}/{scope}",
                "X-Amz-Date": date,
                "X-Amz-Expires": "60",
                "X-Amz-SignedHeaders": "host",
            }
        )

        body_hash = "UNSIGNED-PAYLOAD"
    else:
        headers.update({"x-amz-content-sha256": body_hash, "x-amz-date": date})

    if extra_headers:
        headers.update(extra_headers)
        body_hash = headers.get("x-amz-content-sha256", body_hash)

    signed_headers = ";".join(sorted(headers))

    query = "&".join(
        f"{quote(name, safe='~')}={quote(value, safe='~')}"
        for name, value in sorted(fields.items())
    )

    canonical_headers = "".join(f"{name}:{headers[name]}\n" for name in sorted(headers))

    canonical_request = (
        f"{method}\n{canonical_path}\n{query}\n{canonical_headers}\n"
        f"{signed_headers}\n{body_hash}"
    )

    scope, request_signature = sign_request(
        canonical_request, date, region, service, secret
    )

    if query:
        path += f"?{query}"

    if mode in ("query", "expired", "raw-path-query"):
        path += f"&X-Amz-Signature={request_signature}"
    else:
        headers["authorization"] = (
            f"AWS4-HMAC-SHA256 Credential={key}/{scope}, "
            f"SignedHeaders={signed_headers}, Signature={request_signature}"
        )

    headers.update({"content-length": str(len(payload)), "connection": "close"})

    request_head = (
        f"{method} {path} HTTP/1.1\r\n"
        + "".join(f"{name}: {value}\r\n" for name, value in headers.items())
        + "\r\n"
    )

    return request_head.encode() + payload


def build_attributes_request(mode: str) -> bytes:
    """Build an attribute-selector probe, including missing or invalid selectors."""
    headers = {
        "etag": {"x-amz-object-attributes": "ETag"},
        "size": {"x-amz-object-attributes": "ObjectSize"},
        "count-only": {"x-amz-object-attributes": "ObjectParts"},
        "part": {
            "x-amz-object-attributes": "ObjectParts,Checksum",
            "x-amz-max-parts": "1",
            "x-amz-part-number-marker": "0",
        },
        "condition": {"x-amz-object-attributes": "ETag", "if-match": "*"},
        "unknown": {"x-amz-object-attributes": "Unknown"},
        "empty": {"x-amz-object-attributes": ""},
        "duplicate": {"x-amz-object-attributes": "ETag,ETag"},
        "empty-element": {"x-amz-object-attributes": "ETag,,Checksum"},
        "missing": {},
    }[mode]

    return request(
        method="GET",
        path=f"/photos/{mode}",
        mode="header",
        key=PROBE_ACCESS_KEY_ID,
        secret=PROBE_SECRET_ACCESS_KEY,
        region="us-east-1",
        service="s3",
        payload=b"",
        extra_headers=headers,
        query_fields={"attributes": ""},
    )


def build_response_phase_request(operation: str, mode: str) -> bytes:
    """Build a copy or completion probe with the selected body-framing variant."""
    headers = {}
    query = {}
    payload = b""

    if operation == "complete":
        query = {"uploadId": "synthetic-upload"}

        payload = (
            b"<CompleteMultipartUpload><Part><PartNumber>1</PartNumber>"
            b'<ETag>"25f9e794323b453885f5181f1b624d0b"</ETag>'
            b"</Part></CompleteMultipartUpload>"
        )
    else:
        headers["x-amz-copy-source"] = "/photos/source"

        if operation == "part":
            query = {"uploadId": "synthetic-upload", "partNumber": "1"}

        if mode == "nonempty":
            payload = b"unexpected"

    raw = request(
        method="POST" if operation == "complete" else "PUT",
        path=f"/photos/{mode}",
        mode="header",
        key=PROBE_ACCESS_KEY_ID,
        secret=PROBE_SECRET_ACCESS_KEY,
        region="us-east-1",
        service="s3",
        payload=payload,
        extra_headers=headers,
        query_fields=query,
    )

    if mode.startswith("chunked-"):
        raw = raw.replace(b"content-length: 0\r\n", b"transfer-encoding: chunked\r\n")

        raw += {
            "chunked-empty": b"0\r\n\r\n",
            "chunked-byte": b"1\r\nx\r\n0\r\n\r\n",
            "chunked-truncated": b"0\r\n",
            "chunked-invalid": b"invalid\r\n",
            "chunked-stalled": b"",
        }[mode]

    return raw


def build_object_values_request(mode: str) -> bytes:
    """Build metadata and representation probes without sanitizing invalid values."""
    if mode.startswith("metadata-repeated"):
        first, second = {
            "metadata-repeated": ("first", "second"),
            "metadata-repeated-empty": ("", "second"),
            "metadata-repeated-unicode": ("=?UTF-8?B?Y2Fmw6k=?=", "second"),
            "metadata-repeated-limit": ("x" * 1022, "y" * 1023),
            "metadata-repeated-over": ("x" * 1022, "y" * 1024),
        }[mode]

        combined = f"{first},{second}"

        raw = request(
            method="PUT",
            path="/photos/value-check",
            mode="header",
            key=PROBE_ACCESS_KEY_ID,
            secret=PROBE_SECRET_ACCESS_KEY,
            region="us-east-1",
            service="s3",
            payload=b"",
            extra_headers={"x-amz-meta-id": combined},
        )

        return raw.replace(
            f"x-amz-meta-id: {combined}\r\n".encode(),
            f"X-Amz-Meta-Id: {first}\r\nx-amz-meta-id: {second}\r\n".encode(),
        )

    headers = {
        "metadata-unicode": {"x-amz-meta-title": "=?UTF-8?B?Y2Fmw6k=?="},
        "metadata-empty": {"x-amz-meta-title": ""},
        "metadata-count-limit": {f"x-amz-meta-k{index:03}": "" for index in range(64)},
        "metadata-count-over": {f"x-amz-meta-k{index:03}": "" for index in range(65)},
        "metadata-limit": {"x-amz-meta-id": "x" * 2046},
        "metadata-over": {"x-amz-meta-id": "x" * 2047},
        "metadata-control": {"x-amz-meta-title": "=?UTF-8?B?YQpi?="},
        "metadata-empty-key": {"x-amz-meta-": "value"},
        "representation-limit": {"cache-control": "x" * (2048 - 13)},
        "representation-over": {"cache-control": "x" * (2048 - 12)},
        "representation-unicode": {"content-disposition": "café"},
        "expires-early": {"expires": "Mon, 01 Jan 0001 00:00:00 GMT"},
        "expires-invalid": {"expires": "not a date"},
        "expires-outside-range": {"expires": "Sat, 01 Jan 0000 00:00:00 GMT"},
    }[mode]

    return request(
        method="PUT",
        path="/photos/value-check",
        mode="header",
        key=PROBE_ACCESS_KEY_ID,
        secret=PROBE_SECRET_ACCESS_KEY,
        region="us-east-1",
        service="s3",
        payload=b"",
        extra_headers=headers,
    )


def integrity(mode: str) -> bytes:
    """Independently hashed bytes, with deliberately invalid terminal variants."""
    if mode.startswith("flex:"):
        _, algorithm, transport = mode.split(":", 2)
        return build_flexible_integrity_request(algorithm, transport)

    if mode.startswith("signed-"):
        return build_signed_stream_request(mode)

    body = b"" if mode == "empty" else b"123456789"
    crc32 = base64.b64encode(zlib.crc32(body).to_bytes(4, "big")).decode()
    headers = {"x-amz-checksum-crc32": crc32}

    if mode == "bad-header":
        headers["x-amz-checksum-crc32"] = "AAAAAA=="

    if mode == "bad-md5":
        headers["content-md5"] = base64.b64encode(
            hashlib.md5(b"wrong").digest()
        ).decode()

    if mode not in ("header", "bad-header", "bad-md5", "empty"):
        terminal = f"x-amz-checksum-crc32:{crc32}\r\n\r\n".encode()

        if mode == "bad-trailer":
            terminal = b"x-amz-checksum-crc32:AAAAAA==\r\n\r\n"

        if mode == "missing-trailer":
            terminal = b"\r\n"

        if mode == "truncated-trailer":
            terminal = terminal[:-3]

        if mode == "missing-final-crlf":
            terminal = terminal[:-2]

        if mode == "trailer-lf":
            terminal = terminal[:-4] + b"\n\r\n\r\n"

        if mode == "duplicate-trailer":
            terminal = terminal[:-2] + b"x-amz-checksum-crc32:AAAAAA==\r\n\r\n"

        headers = {
            "content-encoding": "aws-chunked",
            "x-amz-content-sha256": "STREAMING-UNSIGNED-PAYLOAD-TRAILER",
            "x-amz-decoded-content-length": str(len(body)),
            "x-amz-trailer": "x-amz-checksum-crc32",
        }

        if mode == "extra-decoded":
            headers["x-amz-decoded-content-length"] = "8"

        if mode == "short-decoded":
            headers["x-amz-decoded-content-length"] = "10"

        body = b"9\r\n" + body + b"\r\n0\r\n" + terminal

        if mode == "missing-completion":
            body = b"9\r\n123456789\r\n"

    return request(
        method="PUT",
        path="/photos/integrity",
        mode="header",
        key=PROBE_ACCESS_KEY_ID,
        secret=PROBE_SECRET_ACCESS_KEY,
        region="us-east-1",
        service="s3",
        payload=body,
        extra_headers=headers,
    )


def build_flexible_integrity_request(algorithm: str, transport: str) -> bytes:
    """Use independent fixed vectors for each header/trailer algorithm."""
    vectors_path = Path(__file__).with_name("checksum-vectors.json")
    vectors = json.loads(vectors_path.read_text(encoding="utf-8"))
    body = b"123456789"
    checksum_header = "x-amz-checksum-" + algorithm.lower()
    checksum = vectors["vectors"][1]["checksums"][algorithm]

    if transport.endswith("bad"):
        checksum = vectors["vectors"][0]["checksums"][algorithm]

    headers = {checksum_header: checksum}

    if transport.startswith("trailer"):
        terminal = f"{checksum_header}:{checksum}\r\n\r\n".encode()

        if transport.endswith("truncated"):
            terminal = terminal[:-1]

        headers = {
            "content-encoding": "aws-chunked",
            "x-amz-content-sha256": "STREAMING-UNSIGNED-PAYLOAD-TRAILER",
            "x-amz-decoded-content-length": "9",
            "x-amz-trailer": checksum_header,
        }

        body = b"9\r\n" + body + b"\r\n0\r\n" + terminal

    return request(
        method="PUT",
        path="/photos/integrity",
        mode="header",
        key=PROBE_ACCESS_KEY_ID,
        secret=PROBE_SECRET_ACCESS_KEY,
        region="us-east-1",
        service="s3",
        payload=body,
        extra_headers=headers,
    )


def build_signed_stream_request(mode: str) -> bytes:
    """Chain independent signatures, including the mandatory zero-byte chunk."""
    chunks = [] if mode == "signed-empty" else [b"1234", b"56789"]

    raw = request(
        method="PUT",
        path="/photos/integrity",
        mode="header",
        key=PROBE_ACCESS_KEY_ID,
        secret=PROBE_SECRET_ACCESS_KEY,
        region="us-east-1",
        service="s3",
        payload=b"",
        extra_headers={
            "content-encoding": "aws-chunked",
            "x-amz-content-sha256": "STREAMING-AWS4-HMAC-SHA256-PAYLOAD",
            "x-amz-decoded-content-length": str(sum(map(len, chunks))),
        },
    )

    lines = raw.decode().split("\r\n")

    date = next(
        line.split(": ", 1)[1] for line in lines if line.startswith("x-amz-date:")
    )

    previous = next(
        line.rsplit("Signature=", 1)[1]
        for line in lines
        if line.startswith("authorization:")
    )

    framed_chunks = []

    for payload in [*chunks, b""]:
        previous = sign_chunk(payload, previous, date, PROBE_SECRET_ACCESS_KEY)

        encoded = (
            f"{len(payload):x};chunk-signature={previous}\r\n".encode()
            + payload
            + b"\r\n"
        )

        framed_chunks.append(encoded)

    if mode == "signed-missing-completion":
        framed_chunks.pop()

    if mode == "signed-bad-completion":
        framed_chunks[-1] = b"0;chunk-signature=" + b"0" * 64 + b"\r\n\r\n"

    if mode == "signed-bad-data":
        framed_chunks[0] = framed_chunks[0].replace(b"1234", b"123X")

    if mode == "signed-reordered":
        framed_chunks[0], framed_chunks[1] = framed_chunks[1], framed_chunks[0]

    body = b"".join(framed_chunks)

    if mode == "signed-truncated":
        body = body[:-1]

    if mode == "signed-extra-bytes":
        body += b"unexpected\r\n\r\n"

    return (
        raw.replace(
            b"\r\ncontent-length: 0\r\n",
            f"\r\ncontent-length: {len(body)}\r\n".encode(),
        )
        + body
    )


def build_post_request(mode: str) -> bytes:
    """Small signed form; malformed modes keep framing length accurate."""
    now = datetime.now(UTC)
    date = now.strftime("%Y%m%dT%H%M%SZ")
    scope = f"{date[:8]}/us-east-1/s3/aws4_request"

    fields = {
        "key": "album/${filename}",
        "x-amz-algorithm": "AWS4-HMAC-SHA256",
        "x-amz-credential": f"{PROBE_ACCESS_KEY_ID}/{scope}",
        "x-amz-date": date,
    }

    extra_fields = {
        "field-unknown-amz": ("x-amz-future-option", "required"),
        "field-encryption": ("x-amz-server-side-encryption", "AES256"),
        "field-owner": ("x-amz-expected-bucket-owner", "123456789012"),
        "field-empty-metadata-name": ("x-amz-meta-", "ignored"),
        "field-if-match": ("if-match", '"expected"'),
        "field-if-none-match": ("If-None-Match", "*"),
        "field-content-length": ("content-length", "7"),
        "field-content-language": ("content-language", "en"),
        "field-toolkit": ("client-note", "policy-covered toolkit value"),
        "field-ignored": ("x-ignore-note", "ignored transport value"),
        "field-storage-class": ("x-amz-storage-class", "STANDARD"),
    }

    if mode in extra_fields:
        name, value = extra_fields[mode]
        fields[name] = value

    if mode in ("policy-content-types", "policy-content-types-mismatch"):
        fields["Content-Type"] = (
            "image/png,image/jpeg"
            if mode == "policy-content-types"
            else "image/png,text/plain"
        )

    if mode == "policy-empty-metadata":
        fields["x-amz-meta-note"] = ""

    if mode.startswith("form-") and mode != "form-fields-absent":
        acl_field, tagging_field = "acl", "tagging"

        if mode == "form-fields-mixed-case":
            acl_field, tagging_field = "AcL", "TaGgInG"

        if mode == "form-put-acl":
            acl_field = "x-amz-acl"

        if mode == "form-put-tagging":
            tagging_field = "x-amz-tagging"

        fields[acl_field] = "bucket-owner-full-control"

        fields[tagging_field] = (
            ""
            if mode == "form-fields-empty"
            else (
                "<Tagging><TagSet><Tag><Key>Label</Key><Value>café</Value>"
                "</Tag></TagSet></Tagging>"
            )
        )

        fields["Content-Type"] = "image/png"

        fields["x-amz-meta-note"] = (
            "" if mode == "form-fields-empty" else "form metadata"
        )

    if mode in ("checksums", "bad-form-md5", "bad-form-crc32"):
        md5_payload = b"wrong" if mode == "bad-form-md5" else b"payload"
        crc32_payload = b"wrong" if mode == "bad-form-crc32" else b"payload"

        fields["Content-MD5"] = base64.b64encode(
            hashlib.md5(md5_payload).digest()
        ).decode()

        fields["x-amz-checksum-algorithm"] = "CRC32"

        fields["x-amz-checksum-crc32"] = base64.b64encode(
            zlib.crc32(crc32_payload).to_bytes(4, "big")
        ).decode()

    if mode in ("201", "201-encoded-key", "bad-redirect-201", "bad-status"):
        fields["success_action_status"] = "garbage" if mode == "bad-status" else "201"

    if mode == "201-encoded-key":
        fields["key"] = "album/../% +?#&雪//${filename}"

    if mode in (
        "bad-redirect",
        "bad-redirect-201",
        "redirect-alias",
        "redirect",
        "redirect-control",
        "redirect-truncated",
    ):
        redirect = "https://example.invalid/done?existing=1#done"

        if mode.startswith("bad-redirect"):
            redirect = "not a URL"

        if mode == "redirect-control":
            redirect += "\r\nInjected: value"

        fields[
            "redirect" if mode == "redirect-alias" else "success_action_redirect"
        ] = redirect

    durable_fields = {
        "durable-empty-tags": {"tagging": "<Tagging><TagSet/></Tagging>"},
        "durable-tag-tail": {"tagging": "<Tagging><TagSet/></Tagging>tail"},
        "durable-empty-md5": {"Content-MD5": ""},
        "durable-algorithm-only": {"x-amz-checksum-algorithm": "SHA256"},
        "durable-algorithm-mismatch": {
            "x-amz-checksum-algorithm": "SHA1",
            "x-amz-checksum-crc32": "QixqFQ==",
        },
        "durable-empty-type": {"Content-Type": "", "x-amz-meta-empty": ""},
        "durable-private-acl": {"acl": "private"},
    }

    fields.update(durable_fields.get(mode, {}))

    if mode == "form-expansion-over-limit":
        fields["key"] = "album/" + "${filename}" * 600

    conditions: list[dict[str, str] | list[str | int]] = [
        {"bucket": "photos"},
        ["starts-with", "$key", "album/"],
    ]

    if mode.startswith("range-"):
        conditions.append(["content-length-range", 4, 10])

        conditions.append(
            [
                "content-length-range",
                *{
                    "range-at-limit": (7, 7),
                    "range-too-short": (8, 10),
                    "range-too-long": (1, 6),
                    "range-disjoint": (11, 12),
                }[mode],
            ]
        )

    conditions.extend({key: value} for key, value in fields.items() if key != "key")

    if mode in ("policy-content-types", "policy-content-types-mismatch"):
        conditions.append(["starts-with", "$Content-Type", "image/"])

    if mode in ("policy-empty-metadata", "policy-missing-metadata"):
        conditions.append(["starts-with", "$x-amz-meta-note", ""])

    if mode == "policy-missing-bucket":
        conditions = conditions[1:]

    if mode == "policy-bucket-prefix":
        conditions[0] = ["starts-with", "$bucket", "photos"]

    if mode == "policy-signing-prefix":
        conditions = [
            condition
            for condition in conditions
            if not isinstance(condition, dict) or "x-amz-date" not in condition
        ]

        conditions.append(["starts-with", "$x-amz-date", ""])

    if mode == "policy-unknown-operator":
        conditions.append(["EQ", "$key", "album/a.txt"])

    if mode == "policy-invalid-field-reference":
        conditions.append(["eq", "key", "album/a.txt"])

    policy = {
        "expiration": (now + timedelta(minutes=5)).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "conditions": conditions,
    }

    policy_json = json.dumps(policy, separators=(",", ":"))

    if mode == "policy-duplicate-member":
        policy_json = policy_json.replace(
            '"bucket":"photos"', '"bucket":"other","bucket":"photos"'
        )

    if mode == "policy-duplicate-expiration":
        policy_json = policy_json.replace(
            '"expiration":', '"expiration":"2000-01-01T00:00:00Z","expiration":'
        )

    if mode == "policy-duplicate-conditions":
        policy_json = policy_json.replace(
            '"conditions":', '"conditions":[],"conditions":'
        )

    invalid_conditions = {
        "policy-condition-boolean": '{"x-ignore-value":true}',
        "policy-range-boolean": '["content-length-range",false,7]',
        "policy-range-fraction": '["content-length-range",0,7.5]',
        "policy-range-negative": '["content-length-range",-1,7]',
        "policy-range-overflow": '["content-length-range",0,18446744073709551616]',
        "policy-range-reversed": '["content-length-range",8,7]',
        "policy-deep-condition": "[" * 130 + "0" + "]" * 130,
    }

    if mode in invalid_conditions:
        policy_json = policy_json.replace(
            '"conditions":[', f'"conditions":[{invalid_conditions[mode]},'
        )

    if mode == "policy-field-over-limit":
        policy_json += " " * 20_480

    fields["policy"] = base64.b64encode(policy_json.encode()).decode()
    signing_key = ("AWS4" + PROBE_SECRET_ACCESS_KEY).encode()

    for scope_part in (date[:8], "us-east-1", "s3", "aws4_request"):
        signing_key = hmac.digest(signing_key, scope_part.encode(), "sha256")

    fields["x-amz-signature"] = hmac.new(
        signing_key, fields["policy"].encode(), "sha256"
    ).hexdigest()

    if mode == "bad-signature":
        fields["x-amz-signature"] = "0" * 64

    boundary = "mikrio-independent-form"

    def encode_field(name: str, value: str) -> str:
        return (
            f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n'
            f"\r\n{value}\r\n"
        )

    body = "".join(encode_field(name, value) for name, value in fields.items())

    if mode == "duplicate":
        body += encode_field("KEY", "other")

    if mode == "form-parts-over-limit":
        body += "".join(encode_field(f"x-ignore-{index}", "") for index in range(64))

    filename = "x" * 64 if mode == "form-expansion-over-limit" else "a.txt"

    body += (
        f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{filename}"\r\n'
        "Content-Type: text/plain\r\n\r\npayload"
    )

    if mode == "extra-file":
        body += (
            f"\r\n--{boundary}\r\n"
            'Content-Disposition: form-data; name="file"; filename="b.txt"\r\n'
            "\r\nsecond"
        )

    if mode in (
        "ignored-tail",
        "submit-tail",
        "tail-truncated",
        "tail-over-limit",
        "tail-security-field",
    ):
        tail_name = "key" if mode == "tail-security-field" else "submit"

        tail_value = (
            "x" * 20_480 if mode == "tail-over-limit" else "bounded ignored text"
        )

        body += (
            f'\r\n--{boundary}\r\nContent-Disposition: form-data; name="{tail_name}"\r\n'
            f"\r\n{tail_value}"
        )

    body += f"\r\n--{boundary}--\r\n"

    if mode in ("envelope-at-limit", "envelope-over-limit"):
        empty_field = encode_field("x-ignore-padding", "")

        padding_bytes = (
            20_480 - (len(body.encode()) - len("payload")) - len(empty_field.encode())
        )

        if mode == "envelope-over-limit":
            padding_bytes += 1

        assert padding_bytes > 0
        body = encode_field("x-ignore-padding", "x" * padding_bytes) + body

    if mode in ("truncated", "tail-truncated", "redirect-truncated"):
        body = body[:-5]

    if mode == "epilogue":
        body += "forbidden tail"

    raw = (
        "POST /photos HTTP/1.1\r\nhost: localhost:9000\r\n"
        f"content-type: multipart/form-data; boundary={boundary}\r\n"
        f"content-length: {len(body.encode())}\r\nconnection: close\r\n\r\n"
    )

    if mode == "field-outer-if-match":
        raw = raw.replace(
            "connection: close\r\n", 'if-match: "expected"\r\nconnection: close\r\n'
        )

    if mode == "field-outer-if-none-match":
        raw = raw.replace(
            "connection: close\r\n", "if-none-match: *\r\nconnection: close\r\n"
        )

    return (raw + body).encode()


def main() -> None:
    arguments = sys.argv[1:]

    if arguments == ["--check-vector"]:
        check_known_vectors()
        return

    if arguments[0] == "--post":
        raw = build_post_request(arguments[1])
    elif arguments[0] == "--integrity":
        raw = integrity(arguments[1])
    elif arguments[0] == "--values":
        raw = build_object_values_request(arguments[1])
    elif arguments[0] == "--attributes":
        raw = build_attributes_request(arguments[1])
    elif arguments[0] == "--response-phase":
        raw = build_response_phase_request(arguments[1], arguments[2])
    else:
        method, path, mode, access_key_id, secret, region, service, payload = arguments

        raw = request(
            method=method,
            path=path,
            mode=mode,
            key=access_key_id,
            secret=secret,
            region=region,
            service=service,
            payload=payload.encode(),
        )

    sys.stdout.buffer.write(raw)


if __name__ == "__main__":
    main()
