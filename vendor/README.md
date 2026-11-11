# Maintained dependencies

These sources are required for the current storage and HTTP implementation.
Their package names distinguish them from upstream crates. Original license
texts, upstream manifests, and upstream source attribution are retained.

- **`mikrio-hyper`** (hyper 1.12.0): Reject conflicting HTTP/1 request framing
  before dispatch.
- **`mikrio-hyper-util`** (hyper-util 0.1.21): Select the maintained Hyper
  dependency.
- **`mikrio-s3s`** (s3s 0.17.0): Strict request/signature interpretation,
  complete body framing, configuration XML validation, POST/copy/part-read
  integration, request identifiers, and maintained dependency edges.
- **`mikrio-s3s-multipart`** (s3s-multipart 0.17.0): Bound streamed
  form-envelope parsing and validate the form tail.
- **`mikrio-libsqlite3-sys`** (libsqlite3-sys 0.39.1): Build one shared bundled
  SQLite runtime with isolated symbols and a stable loader identity.
- **`mikrio-rusqlite`** (rusqlite 0.41.1): Select the maintained SQLite
  dependency.

The versions retain the upstream base with the `-mikrio.1` suffix. They are
separate from the coordinated project version. `Cargo.toml.orig` preserves the
upstream-oriented manifest; each maintained manifest declares its upstream name
and version under `package.metadata.mikrio`.

Keep changes small and preserve unrelated upstream source and formatting. Review
upstream security updates under their original identities. Remove a maintained
distribution when an upstream graph supplies and passes the required behavior,
including mixed native-image SQLite coordination. A dependency rename does not
change its licensing or remove the need for upstream security review.

Some s3s operations use templates beside the upstream code generator. Check
generated output with:

```sh
python3 -m tools.regenerate_s3s --check
```

Omit `--check` to regenerate after a template change. Tests in the HTTP adapter
exercise the shared parser and storage behavior.

[`licenses/`](licenses/) contains exact upstream license texts omitted from some
released dependency archives. [`licenses/sources.json`](licenses/sources.json)
records their source URLs and checksums. `tools.native_notices` combines those
with the locked target graph to generate notices for native distributions.
