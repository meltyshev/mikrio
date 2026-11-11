# Support

Report reproducible bugs at
[GitHub issues](https://github.com/meltyshev/mikrio/issues). Include the exact
package versions, runtime, operating system, filesystem, expected and actual
behavior, and a small reproduction. Redact credentials, signed requests, object
contents, and private paths. Use [private reporting](SECURITY.md) for security
issues.

## Platforms

The distribution targets are:

| Platform           | Rust target                 | Target filesystem |
| ------------------ | --------------------------- | ----------------- |
| macOS arm64        | `aarch64-apple-darwin`      | APFS              |
| macOS x86_64       | `x86_64-apple-darwin`       | APFS              |
| Linux glibc arm64  | `aarch64-unknown-linux-gnu` | ext4              |
| Linux glibc x86_64 | `x86_64-unknown-linux-gnu`  | ext4              |
| Windows x86_64     | `x86_64-pc-windows-msvc`    | NTFS              |

Compatibility with macOS x86_64, Linux x86_64, and Windows x86_64 is expected
but unverified for 0.1.0. Release notes list tested systems and minimum versions
for the remaining targets. Cross-compilation alone does not establish runtime
support.

The runtime matrix covers Node.js 22, 24, and 26 and standard CPython 3.13,
3.14, and 3.15. Rust manifests require 1.96 or newer; the contributor toolchain
is pinned in `rust-toolchain.toml`. Free-threaded Python, subinterpreters, PyPy,
alternative JavaScript runtimes, musl, mobile systems, and a browser-hosted
engine are outside this matrix. Package metadata may describe a broader binary
compatibility floor than the runtimes exercised for a release.

Use local storage on one host. Network shares, cloud-synchronized directories,
and concurrent access from different hosts are unsupported. Successful opening
does not certify the filesystem or provide a full integrity scan.

## Maintenance

The current release line receives best-effort maintenance, prioritizing
data-loss and security fixes. There is no LTS, response-time, availability, or
older-line backport commitment. Before 1.0, language API changes are documented
in release notes. Storage upgrades follow the separate
[migration policy](docs/storage.md#format-and-migration).
