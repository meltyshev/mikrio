> mikrio-maintained distribution `mikrio-s3s` (0.17.0-mikrio.1). Based on
> upstream `s3s` 0.17.0; the upstream text and attribution follow. This package
> is an internal implementation dependency, not a supported mikrio API. See
> [maintained dependencies][maintained-dependencies] for the patch description
> and upstream sources.

# s3s

[![Apache 2.0 licensed][license-badge]][license-url]
[![Unsafe Forbidden][unsafe-forbidden-badge]][unsafe-forbidden-url]

[license-badge]: https://img.shields.io/badge/license-Apache--2.0-blue.svg
[license-url]: ./LICENSE
[unsafe-forbidden-badge]:
  https://img.shields.io/badge/unsafe-forbidden-success.svg
[unsafe-forbidden-url]: https://github.com/rust-secure-code/safety-dance/

S3 Service Adapter

- [s3s](https://github.com/s3s-project/s3s/tree/main/crates/s3s/):
  [![Crates.io][crates-io-7]][crates-io]
  [![Docs](https://docs.rs/s3s/badge.svg)](https://docs.rs/s3s/)
- [s3s-aws][s3s-aws]: [![Crates.io][crates-io-8]][crates-io-2]
  [![Docs](https://docs.rs/s3s-aws/badge.svg)][docs]
- [s3s-sigv2][s3s-sigv2]: [![Crates.io][crates-io-9]][crates-io-3]
  [![Docs](https://docs.rs/s3s-sigv2/badge.svg)][docs-2]
- [s3s-sigv4][s3s-sigv4]: [![Crates.io][crates-io-10]][crates-io-4]
  [![Docs](https://docs.rs/s3s-sigv4/badge.svg)][docs-3]
- [s3s-rfc2047][s3s-rfc2047]: [![Crates.io][crates-io-11]][crates-io-5]
  [![Docs](https://docs.rs/s3s-rfc2047/badge.svg)][docs-4]
- [s3s-fs][s3s-fs]: [![Crates.io][crates-io-12]][crates-io-6]
  [![Docs](https://docs.rs/s3s-fs/badge.svg)][docs-5]

**[Development documentation](https://s3s-project.github.io/s3s/)** for the
`main` branch is available on GitHub Pages.

This experimental project intends to offer an ergonomic adapter for building
S3-compatible services.

`s3s` implements Amazon S3 REST API in the form of a generic
[hyper](https://github.com/hyperium/hyper) service. S3-compatible services can
focus on the S3 API itself and don't have to care about the HTTP layer.

`s3s-aws` provides useful types and integration with
[`aws-sdk-s3`](https://crates.io/crates/aws-sdk-s3).

`s3s-rfc2047` provides RFC 2047 MIME encoded-word encoding and decoding for
non-ASCII header values.

`s3s-fs` implements the S3 API based on file system, as a sample implementation.
It is designed for integration testing, which can be used to [mock an S3
client][mock-an-s3-client]. It also provides a binary for debugging. [Play
it!][play-it]

## How it works

![architecture diagram][architecture-diagram]

The diagram above shows how `s3s` works.

`s3s` converts HTTP requests to operation inputs before calling the user-defined
service.

`s3s` converts operation outputs or errors to HTTP responses after calling the
user-defined service.

The data types, serialization and deserialization are generated from the smithy
model in [aws-sdk-rust](https://github.com/awslabs/aws-sdk-rust) repository. We
apply manual hacks to fix some problems in [smithy server
codegen][smithy-server-codegen] and make `s3s` ready to use now.

## Security

`S3Service` and other adapters in this project are not a complete security
boundary. If they are exposed to the Internet directly, they may be
**attacked**.

It is up to the user to implement security enhancements such as **HTTP body
length limits**, object-size limits, rate limits and back pressure.

**Authentication is required for production deployments.** Without calling
`set_auth`, the service accepts anonymous (unsigned) requests and skips
authorization entirely: every S3 operation is open to any client that can reach
the service, and signed requests fail with `NotImplemented` because no
authentication provider is configured. A forgotten `set_auth` turns the service
into a publicly readable and writable endpoint.

For streaming uploads (`PUT Object`, `UploadPart`), `s3s` applies a default 5
GiB object-size limit matching the AWS single-PUT limit; set
`S3Config::put_object_max_size` to `None` to disable it and enforce
deployment-specific caps in the `S3` implementation. For production, set it
explicitly even though the default is already 5 GiB. `POST Object` keeps using
`S3Config::post_object_max_file_size`.

List-type responses (`ListObjects`, `ListBuckets`, ...) are serialized in full
by `s3s`: their memory usage grows with the number of entries the `S3`
implementation returns. Implementations should paginate (`max-keys` /
continuation tokens) and deployments should bound response sizes.

## Docker

Docker images are available at [GitHub Container Registry
(GHCR)][github-container-registry-gh].

See [Docker documentation][docker-documentation] for usage details.

## Contributing

- [Development Guide][development-guide]

## Sponsor

We have a reward funds pool for contributors:
<https://github.com/Nugine/s3s/issues/174>

If my open-source work has been helpful to you, please
[sponsor me](https://github.com/Nugine#sponsor).

Every little bit helps. Thank you!

[maintained-dependencies]: https://github.com/meltyshev/mikrio/tree/main/vendor
[crates-io]: https://crates.io/crates/s3s
[s3s-aws]: https://github.com/s3s-project/s3s/tree/main/crates/s3s-aws/
[crates-io-2]: https://crates.io/crates/s3s-aws
[docs]: https://docs.rs/s3s-aws/
[s3s-sigv2]: https://github.com/s3s-project/s3s/tree/main/crates/s3s-sigv2/
[crates-io-3]: https://crates.io/crates/s3s-sigv2
[docs-2]: https://docs.rs/s3s-sigv2/
[s3s-sigv4]: https://github.com/s3s-project/s3s/tree/main/crates/s3s-sigv4/
[crates-io-4]: https://crates.io/crates/s3s-sigv4
[docs-3]: https://docs.rs/s3s-sigv4/
[s3s-rfc2047]: https://github.com/s3s-project/s3s/tree/main/crates/s3s-rfc2047/
[crates-io-5]: https://crates.io/crates/s3s-rfc2047
[docs-4]: https://docs.rs/s3s-rfc2047/
[s3s-fs]: https://github.com/s3s-project/s3s/tree/main/crates/s3s-fs/
[crates-io-6]: https://crates.io/crates/s3s-fs
[docs-5]: https://docs.rs/s3s-fs/
[mock-an-s3-client]:
  https://github.com/s3s-project/s3s/tree/main/crates/s3s-fs/tests
[play-it]:
  https://github.com/s3s-project/s3s/blob/main/CONTRIBUTING.md#play-the-test-server
[architecture-diagram]:
  https://raw.githubusercontent.com/s3s-project/s3s/main/docs/arch/arch.svg
[smithy-server-codegen]:
  https://smithy-lang.github.io/smithy-rs/design/server/overview.html
[github-container-registry-gh]:
  https://github.com/s3s-project/s3s/pkgs/container/s3s
[docker-documentation]:
  https://github.com/s3s-project/s3s/blob/main/docs/docker.md
[development-guide]:
  https://github.com/s3s-project/s3s/blob/main/CONTRIBUTING.md
[crates-io-7]: https://img.shields.io/crates/v/s3s.svg
[crates-io-8]: https://img.shields.io/crates/v/s3s-aws.svg
[crates-io-9]: https://img.shields.io/crates/v/s3s-sigv2.svg
[crates-io-10]: https://img.shields.io/crates/v/s3s-sigv4.svg
[crates-io-11]: https://img.shields.io/crates/v/s3s-rfc2047.svg
[crates-io-12]: https://img.shields.io/crates/v/s3s-fs.svg
