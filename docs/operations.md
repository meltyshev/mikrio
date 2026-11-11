# Operations

Use a dedicated directory on a local filesystem, owned by the application
account. Protect backups with the same care as the live store: they contain
object data, security configuration, and persistent signing material. See
[support](../SUPPORT.md) for platform boundaries and [storage](storage.md) for
the managed-directory model.

## CLI

Verify the archive for your platform against `SHA256SUMS`, then extract it into
a new directory. Keep the executable, SQLite companion, runtime receipt, and
notices together. Add that directory to `PATH`. A standalone `cargo install`
does not deploy the required companion and is not the documented CLI
installation route.

```sh
mikrio init ./storage --bucket uploads --json
mikrio serve ./storage --port 9000
```

`init` creates or reopens storage without a listener. `serve` creates or reopens
storage, optionally ensures buckets, and runs until shutdown. Repeating setup
preserves existing region, identity, policies, keys, and objects. Bucket setup
is a sequence of independent commits, so failed startup does not undo earlier
setup.

Run `mikrio --help` and each subcommand's `--help` for the complete arguments.
All administration commands require an existing recognized store.

| Command                           | Purpose                                 |
| --------------------------------- | --------------------------------------- |
| `init`                            | Open a store; optionally add buckets    |
| `serve`                           | Start a foreground S3 listener          |
| `principal create/get/list`       | Create or inspect identities            |
| `principal enable/disable/delete` | Control identity lifetime               |
| `access-key create/list/revoke`   | Manage application credentials          |
| `policy apply/get/delete`         | Read or change identity/bucket policies |

## Application credentials

1. Create a principal with
   `mikrio principal create ./storage --name app --json`.
2. Apply its policy with
   `mikrio policy apply ./storage --principal-id ID --file app-policy.json`. A
   new principal has no grants. The policy replaces the whole document; a
   rejected replacement preserves the previous policy.
3. Issue a key with
   `mikrio access-key create ./storage --principal-id ID --json`. Store the
   returned ID and one-time secret securely.
4. Test allowed and denied requests. Rotate by creating and testing a
   replacement key before revoking the old one. Listing keys cannot recover
   their secrets.

Policy commands take exactly one principal or bucket target. `--file -` reads
stdin explicitly. Principal and key listings return one bounded page at a time.
Deleting a principal requires revoking its remaining keys first.

Native administration can run while another local process serves requests. It
also permits policy recovery after HTTP lockout, without editing SQLite or
creating a separate administrative endpoint.

Generated listener credentials identify a temporary owner and expire with their
listener. Do not distribute them as application credentials. Only `serve` uses
the listener host, port, public origin, and bootstrap credential settings. Flags
take precedence over corresponding lowercase `mikrio_` environment variables.
Credential secrets have no command-line flags and supplied secrets are not
echoed.

`--json` emits schema 1 records. Generated readiness and key-creation output can
contain secrets. Always check the final exit status, including after a result:
output or close can fail after a mutation committed. Exit 0 is success, 2 is an
argument/configuration error, and 1 is an operation/runtime/I/O failure.

Machine output is newline-delimited JSON. stdout carries `result` or `ready`
events; stderr carries `error` or `diagnostic` events. Every record has
`schemaVersion: 1`. Result fields use camelCase, absent optional fields are
omitted, and timestamps use UTC RFC 3339 with three fractional digits and `Z`.
Consumers must tolerate added fields and check the schema version.

Errors use the same codes as the language APIs, including `OperationCanceled`.
Their `phase` identifies `arguments`, `input`, `open`, `operation`, `output`, or
`close`; `command` is omitted when parsing cannot identify it. Match `code` and
`mutationOutcome`, never message text. Command and flag names, environment
names, exit meanings, and JSON schema 1 are compatibility commitments;
incompatible machine output requires a new schema version.

## Shutdown and diagnostics

SIGINT/SIGTERM, or Ctrl+C on Windows, request orderly shutdown. The process
waits for the listener and store to finish closing. There is no implicit
shutdown timeout. Host applications should stop issuing work and URLs, wait for
their tasks and clients to finish, then close bodies, listeners, and stores.

Stopping the listener does not stop direct native users. Closing one connection
leaves independently opened stores usable. An expired application timeout does
not prove native work stopped or make an uncertain mutation safe to repeat.

Host diagnostics are bounded and redacted; repeated maintenance failures and
dropped observations are reported without blocking storage work. Configure the
host's logger to collect diagnostics. Use typed codes and mutation outcomes for
programmatic error handling.

## HTTPS and browser access

The listener serves HTTP. Terminate TLS at a reverse proxy and configure the
application's SDK with the externally reachable origin and path-style
addressing. Set `public_endpoint` / `publicEndpoint` when generated response
URLs need that origin. Keep the internal listener on loopback or a protected
private interface.

Preserve the signed Host, path, query, and headers. Do not normalize object-key
paths, rewrite the signing origin, inject permissive CORS, or infer
authorization from forwarding headers. Configure bucket CORS for the real
application origin. For streamed uploads and long copies, set deliberate
body-size, buffering, and timeout policies. Certificate and hostname
verification must remain enabled.

The [attachment example](../examples/node/attachments/README.md) shows the
application workflow. Test browser behavior in your deployment; remote browser
deployments have not been verified for 0.1.0.

## Backup and recovery

A live directory copy or SQLite-only backup is not a supported backup. Freeze
all access for the affected store and coordinate the application's own database.

1. Stop mutation, URL issuance, cleanup jobs, listeners, and every native user.
   Prevent automatic restarts and new openers throughout the copy. Wait for
   successful shutdown or confirmed process termination.
2. Copy the complete directory into a new empty destination outside the source.
   Include hidden files, the marker, database and existing sidecars, payloads,
   staging, and guard files. Do not tidy or checkpoint the files manually. Do
   not use hard links to mutable source files.
3. Verify a path, size, and content-digest manifest against the frozen source.
   Retain the version, storage format, backup time, and application recovery
   point outside the store. Keep the verified backup unchanged.
4. Restore into another new directory. Never overlay a live store or mix
   database files and payloads from different backups. Verify the manifest
   before opening.
5. Keep the restored application isolated. Normal open can immediately recover
   state and resume expiration. Check expected objects, metadata, configuration,
   and required multipart state through the public API.
6. Review restored principals, policies, public-access settings, and keys. A
   backup can restore previously revoked credentials; revoke or rotate them
   before starting the restored listener. Reconcile application references
   before enabling automatic orphan deletion or ordinary traffic.

Preserve a complete uncleanly stopped store before opening a recovery copy. WAL
files can contain acknowledged writes. Opening is not a read-only forensic mode,
and there is no option to suspend lifecycle expiration. Keep backups on storage
appropriate to the application's device-loss and retention requirements.

## Failure handling

For space, inode, quota, permission, or I/O failures, stop unnecessary producers
and correct the external resource problem. Reclamation itself can require space
for metadata and journals. Never remove engine-owned payloads to free capacity.

For suspected corruption, stop writers, preserve the complete store offline, and
investigate a separate copy. Do not reset the store, remove sidecars, vacuum, or
edit internal rows. Record redacted version, platform, error, and operation
context for [support](../SUPPORT.md).

Before an upgrade, preserve a verified backup and follow the release's format
compatibility instructions. An old binary cannot downgrade a newer store. A
rollback through a pre-upgrade backup also requires reconciling later writes.
