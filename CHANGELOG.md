# Changelog

## 0.1.1

- Node.js and TypeScript applications use `@mikrio/node` with matching native
  packages for each supported platform.

## 0.1.0

Initial release.

- Embedded object storage for Rust, Python, Node.js, and C.
- Optional authenticated S3-compatible HTTP listeners and a standalone CLI.
- Streaming objects, multipart uploads, copying, checksums, metadata, tagging,
  pagination, and conditional requests.
- Local principals, access keys, policies, public access controls, CORS, and
  prefix-based lifecycle rules.
- Transactional metadata, concurrent local access, automatic reclamation, and
  offline backup and recovery procedures.

Language APIs may change before 1.0. C ABI major 1 and storage format 1 are
versioned independently. See [support](SUPPORT.md) for tested platforms and
limits.
