# API and ownership

Rust, Python, Node.js, and C use the same storage engine. The native APIs
provide 40 storage operations, 12 local administration methods, and explicit
store, listener, and body ownership. HTTP adds signed form POST through the same
write path.

## Interfaces

| Language | Entry point                 | I/O model                           |
| -------- | --------------------------- | ----------------------------------- |
| Rust     | `mikrio::Store`             | Async on the caller's Tokio runtime |
| Python   | `mikrio.Store`              | Synchronous; context managers       |
| Node.js  | `Store` from `mikrio`       | Promises and `AbortSignal`          |
| C        | `mikrio_v1_*` in `mikrio.h` | Synchronous; owned handles          |

Rust inputs use builders with private fields. Python operations use keyword-only
arguments and return typed attribute objects. Node.js uses an input object
followed by optional call options, for example
`store.getObject(input, { signal })`. Methods use snake_case in Rust/Python and
camelCase in Node.js.

The Rust crate documentation, Python
[type stubs](../packages/python/mikrio/__init__.pyi), Node.js
[exports](../packages/node/src/index.mts), and C
[header](../crates/mikrio-ffi/include/mikrio.h) define the complete signatures.
The [operation list](compatibility.md#operations) describes shared semantics.

## Opening and closing

`Store.open` creates a missing or empty managed directory and reopens a valid
existing store. An unrelated nonempty directory, unsupported format, or invalid
store is preserved and rejected. Set `create_if_missing` / `createIfMissing` to
false when initialization is not allowed. Opening may recover interrupted work
and resume maintenance; it is not read-only inspection.

A new store defaults to `us-east-1`. An explicitly supplied region must match an
existing store. Relative directories are resolved when opened. Use UTF-8 paths
for portable bindings. Multiple independent connections and processes on the
same host can access the same local directory.

Opening does not start a listener. `store.listen()` returns an owned server
after binding succeeds. It defaults to `127.0.0.1` and an OS-assigned port.
Server properties expose the actual endpoint, persisted region, and listener
credentials. An optional `public_endpoint` / `publicEndpoint` supplies the
external origin for generated object URLs behind a proxy; it does not change
signing or enable TLS.

Close bodies, listeners, and stores explicitly. Closing a listener leaves direct
store access available. Closing a store closes the work and resources attached
to that connection; independent opens remain usable. Rust `Store::clone()`
shares one connection, including its close lifetime.

Closing is idempotent and has no implicit timeout. A failed close retains
ownership and can be retried. Keep the caller's Tokio runtime alive through Rust
close; enable its time driver, and its I/O driver when using HTTP. Report
secondary close failures without losing the original operation error. Finalizers
and Rust `Drop` provide fallback cleanup without confirming completed shutdown.

Do not transfer native handles between processes. Use spawn/exec or initialize
after a safe pre-initialization fork. Fork-without-exec after initialization is
unsupported, including a fresh open through inherited runtime state.

## Bodies, cancellation, and errors

Object bodies are bounded streams. A response owns its selected object
generation and remains valid while a concurrent write replaces or deletes that
key. Consume or explicitly close the body to release its resources. `read_all` /
`readAll` requires a bound; it is not an unbounded convenience read.

Outputs are detached values. Changing a returned map, date, or credential object
does not modify storage. Missing optional inputs are distinct from zero, empty
strings, or empty collections. Node.js accepts omission and `undefined` for
optional fields; `null` is rejected unless a field explicitly permits it.

Storage errors carry a stable error code and, for mutations, an outcome
describing whether the operation committed, did not commit, or has an unknown
result. Cancellation or a lost response can race publication. Inspect the
outcome and reconcile uncertain writes instead of automatically retrying them.
An ETag or payload digest is not a unique identifier for a write attempt.

Batch deletion has per-entry results and is not an all-or-nothing transaction.
Interrupted batches retain indexed progress where available. Releasing a wrapper
does not prove that native work has stopped.

## Access administration

Native directory access is trusted administrative authority. Local
administration creates, lists, enables, disables, and deletes principals;
manages their policies; and creates, lists, and revokes access keys. These
methods are not a remote IAM or STS service.

New principals have no permissions. Apply an identity policy before using their
keys. A newly created secret is returned once; listing cannot recover it. Rotate
keys by creating and verifying a replacement before revoking the old key.
Revoking the last key is required before deleting a principal.

Generated listener credentials are temporary owner authority, scoped to the
listener. They are distinct from persistent application keys, which can be used
across listeners and restarts. Native access must not be given to an untrusted
process whose permissions are meant to be restricted by HTTP policies.

## C embedding

The header uses fixed-width fields, sized byte spans, and opaque handles. Follow
each function's pointer, length, alignment, and output initialization contract.
The implementation can validate lengths and discriminants but cannot prove that
an arbitrary caller pointer is live.

Use the matching release function for every owned result, error, body, server,
and store. Close waits for work to finish and reports failures; release frees
ownership and must not replace a successful close. Callback buffers cannot
outlive their call unless the header expressly transfers ownership. Keep native
code loaded while handles, callbacks, or work can still refer to it.

The archive contains the versioned ABI library, its linker name or Windows
import library, the SQLite companion, and the header. Deploy those files
together.

## Versioning

All released first-party packages share one version. Rust, Python, and Node.js
APIs may evolve before 1.0; incompatible changes must be explained in release
notes. Internal Rust crates are implementation details even when published as
dependencies.

The C ABI uses major version 1 (`mikrio_v1_*`). A future incompatible binary
interface needs a new ABI major. Package versions, ABI versions, storage format
versions, and marker encodings are independent. See the
[migration guide](storage.md#format-and-migration) for upgrade and rollback
limits.
