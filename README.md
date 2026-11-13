# mikrio

Embedded object storage for Rust, Python, Node.js, and C, with an optional
S3-compatible HTTP endpoint.

mikrio keeps objects in a local directory. Applications can use it directly,
start an in-process HTTP listener for an AWS SDK, or run the `mikrio` CLI.
Multiple processes on the same host can open the same store.

- Streaming reads and writes, conditional requests, checksums, and metadata.
- Multipart uploads, object copying, tagging, and paginated listings.
- Local identities and access keys, bucket policies, public access controls,
  CORS, and prefix-based lifecycle rules.
- Persistent storage with transactional metadata, guarded readers, and automatic
  reclamation of retired data.

Use a dedicated directory on a local filesystem. Network filesystems,
cloud-synchronized directories, and access from multiple hosts are unsupported.
See [compatibility](docs/compatibility.md) for the supported S3 subset and
[support](SUPPORT.md) for platform and runtime boundaries.

## Installation

Package and archive names:

| Interface            | Package                                  |
| -------------------- | ---------------------------------------- |
| Rust                 | `mikrio`                                 |
| Python               | `mikrio`                                 |
| Node.js / TypeScript | `@mikrio/node`                           |
| CLI                  | `mikrio-cli-<version>-<platform>.tar.gz` |
| C                    | `mikrio-c-<version>-<platform>.tar.gz`   |

Releases are distributed through
[GitHub Releases](https://github.com/meltyshev/mikrio/releases), crates.io,
PyPI, and npm. Use the matching version of each native package and its bundled
SQLite companion. The CLI and C archives must be extracted together with that
companion. The [build guide](docs/building.md) covers building from source and
deploying Rust applications.

```sh
pip install mikrio
npm install @mikrio/node
cargo add mikrio
```

## Python

```python
from mikrio import Store

with Store.open(directory="./storage") as store:
    store.create_bucket(bucket="photos")
    store.put_object(bucket="photos", key="hello.txt", body=b"Hello, world!")

    with store.listen() as server:
        print(server.endpoint)
        # Configure an S3 client with server.region and server.credentials.
```

## Node.js

```typescript
import { Store } from "@mikrio/node";

const store = await Store.open({ directory: "./storage" });

try {
  await store.createBucket({ bucket: "photos" });
  await store.putObject({
    bucket: "photos",
    key: "hello.txt",
    body: Buffer.from("Hello, world!"),
  });
} finally {
  await store.close();
}
```

## CLI

```sh
mikrio init ./storage --bucket photos
mikrio serve ./storage --port 9000
```

The listener binds to loopback by default. Readiness output includes temporary
owner credentials; keep it private. For applications, create a principal, apply
a scoped policy, and issue a persistent access key as described in the
[operations guide](docs/operations.md).

## Documentation

- [API and ownership](docs/api.md)
- [S3 compatibility](docs/compatibility.md)
- [CLI, deployment, backup, and recovery](docs/operations.md)
- [Storage architecture](docs/storage.md)
- [Building and testing](docs/building.md)
- [Attachment example](examples/node/attachments/README.md)
- [Contributing](CONTRIBUTING.md) and [security reporting](SECURITY.md)

Build the Rust API documentation with `cargo doc -p mikrio --no-deps`. Python
ships type stubs and `py.typed`; Node.js ships TypeScript declarations. The C
interface is declared in [`mikrio.h`](crates/mikrio-ffi/include/mikrio.h).

Language APIs may evolve before 1.0. The C ABI and on-disk format have
independent versioning. See [the compatibility policy](docs/api.md#versioning).

## License

[Apache-2.0](LICENSE). Native distributions include their third-party notices.
Maintained dependencies and their upstream origins are described in
[vendor/README.md](vendor/README.md).
