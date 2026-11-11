# Storage architecture

mikrio stores transactional metadata in SQLite and object bytes in immutable
payload files. The [SQL schema](../crates/mikrio-core/schema.sql) is the source
of truth for the persisted representation. Application code uses the public APIs
rather than querying or editing those files.

## Managed directory

`store.json` identifies the directory and bootstrap state. `metadata.sqlite3`
contains store identity, region, security state, bucket namespaces, logical
object mappings, payload references, multipart state, and maintenance progress.
SQLite sidecars may contain committed state and must be retained with the
directory.

Staging and payload directories contain bytes owned by the engine. Stable guard
files coordinate processes on the same host. Object keys are logical bytewise
names, never filesystem paths. Do not rename, unlink, replace, or hard-link
managed files, including guard files that appear idle.

One store has one persisted region and stable account/owner identities.
Reopening does not generate replacement credentials or silently initialize a
corrupt store. The engine validates directory and file identity and rejects
unsupported locking; it does not reliably classify every network mount or
synchronization agent.

## Publication and reads

A write reserves managed state, streams and validates its input, makes the
payload durable, and publishes its logical mapping in a metadata transaction.
Input errors, failed integrity checks, and rejected conditions cannot publish a
partial replacement. Recovery distinguishes acknowledged state from interrupted
work; it does not treat the presence of a file as proof of a committed object.

Reads select a logical generation and retain the guards needed to use its
immutable payload. Replacing or deleting the key does not invalidate an already
selected body. COPY and multipart completion produce their own destination
payloads and can require additional disk space. Completed multipart objects
retain their part metadata for part reads and object attributes.

Authorization and conditions are rechecked at the appropriate publication
boundary. HTTP operations cannot bypass policy checks through trusted native
entry points. A late error or lost response can leave a committed mutation;
callers must preserve and interpret the reported outcome.

## Reclamation and lifecycle

Logical deletion and physical reclamation are separate. Cleanup uses metadata
references and native guards to prove that bytes are unused; elapsed time alone
does not authorize unlinking a payload. Held bodies and same-shard activity can
delay reclamation. Retired work is resumed through bounded maintenance passes.

Lifecycle expiration and incomplete-upload cleanup use persisted rules and
original ages. They can resume immediately when an old store is reopened.
Closing a connection does not delete all temporary-looking data, abort durable
uploads, or guarantee that the maintenance backlog has drained.

Capacity planning must allow for active writes, multipart parts and assembly,
COPY destinations, old generations held by readers, metadata/WAL growth, retired
garbage, and backups. A full filesystem can also prevent cleanup from
progressing.

## SQLite runtime

Native components share the bundled `mikrio_sqlite3` runtime. Separate SQLite
copies in one process can disagree about coordination for the same store, so the
runtime's loader identity is part of deployment correctness.

Keep the matching companion beside the executable, Node.js addon, Python
extension, or C library, with its original filename. Do not substitute system
SQLite or rename one copy per binding. Runtime compatibility is checked before
creating a store. Upgrade the native components and companion together after
stopping their host processes.

## Format and migration

The initial release creates storage format 1 directly from the schema. There are
no registered migrations. The operational format is recorded in SQLite; marker
encoding and package versions are independent of it. A matching version still
requires the exact supported schema; incompatible stores are preserved and
rejected rather than repaired on open.

The format includes more than table and column names: constraints, indexes,
stored JSON, identifier encodings, payload paths, and the mapping from blobs to
guard shards all participate in compatibility. Format 1 uses 16-byte binary
identities, lowercase hexadecimal path names, 256 first-byte shards, UTF-8
object keys stored as BLOBs, and signed Unix milliseconds. Upload IDs use the
`u1-` encoding with an order-preserving timestamp and random suffix. These
representations are independent of public language type names.

Stored metadata, tags, checksums, CORS rules, and completion receipts use typed
JSON with bounded sizes. Maps have deterministic key order; rule arrays retain
their order. Absent optional values remain distinct from empty values. Policy
documents retain their validated original text. Reads validate stored values
without silently repairing them. In particular, both new and stored tag keys
reject every ASCII case variant of the reserved `aws:` prefix.

The [schema contract](../tests/contract/schema.json) fingerprints the exact
format-1 SQLite definitions, including constraints and indexes. Both the bundled
runtime tests and the independent SQL verifier check it. Preserve this baseline
when adding a successor format; changing the expected fingerprint is not a
substitute for a migration.

Future changes retain the released schema and add a reviewed transition to the
compiled migration registry. A transition updates schema, data, and the format
version in one transaction under exclusive store use. Fresh creation and
upgrades must produce the same validated schema.

Changing a stored JSON shape or meaning also requires a reviewed format
transition, even when the SQL column remains TEXT. Continuation tokens have
their own authenticated, operation-specific encoding versions; treat them as
opaque and preserve their scope and decoding rules when evolving pagination.

An upgrade that requires exclusivity cannot proceed while another process uses
the store. Unknown newer formats are preserved and rejected. Failed migrations
preserve the store without resetting data. There is no automatic storage
downgrade or public repair command.

Before upgrading valuable data, stop all users and preserve a verified complete
offline backup. Open and verify a separate restored copy before admitting normal
traffic. Reverting an executable does not reverse a migrated format; restoring a
pre-upgrade backup also loses writes made after that backup unless reconciled.
See [backup and recovery](operations.md#backup-and-recovery).

## Integrity and incidents

Opening validates identity and required metadata, and reads detect relevant
structural problems. Neither operation is a universal full-payload scrub or
guarantee of detecting every same-length corruption.

For suspected corruption, preserve the store offline and investigate a separate
copy. Follow [failure handling](operations.md#failure-handling) before
attempting recovery.
