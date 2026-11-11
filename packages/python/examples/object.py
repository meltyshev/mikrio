"""Use an installed mikrio package and a dedicated store directory."""

import argparse

from mikrio import Store


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", help="dedicated store directory; data is retained")
    args = parser.parse_args()
    payload = b"Hello from mikrio.\n"

    with Store.open(directory=args.directory) as store:
        store.create_bucket(bucket="example")

        written = store.put_object(
            bucket="example",
            key="hello.txt",
            body=payload,
            content_type="text/plain; charset=utf-8",
            checksum_algorithm="SHA256",
            if_none_match="*",
        )

        selected = store.get_object(
            bucket="example",
            key="hello.txt",
            if_match=written.etag,
            checksum_mode="ENABLED",
        )

        with selected.body as body:
            actual = body.read_all(max_bytes=len(payload))

        if actual != payload or selected.checksum_sha256 != written.checksum_sha256:
            raise RuntimeError("readback differs from the acknowledged upload")

    print(f"verified example/hello.txt {written.etag}")


if __name__ == "__main__":
    main()
