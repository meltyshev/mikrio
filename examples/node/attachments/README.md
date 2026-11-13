# Attachment example

A Node.js and TypeScript application with local accounts, a browser UI, verified
uploads, and background cleanup. It uses the public `@mikrio/node` package to
provision local storage and the AWS SDK for object operations. Uploads keep
their original key when attached; finalization requires no copy or second
upload.

## Local setup

Follow the [build guide](../../../docs/building.md#setup), then build the
Node.js archives and compile the example from the repository root:

```sh
python3 -m tools.build node
node node_modules/typescript-compiler/bin/tsc \
  -p examples/node/attachments/tsconfig.json
release_version=$(python3 -m tools.version show)
npm install --prefix examples/node/attachments/dist \
  --ignore-scripts --omit=optional --no-audit --no-fund \
  "$PWD/dist/mikrio-node-$release_version.tgz" \
  "$PWD/dist/mikrio-node-darwin-arm64-$release_version.tgz"
node examples/node/attachments/dist/src/main.mjs local-init \
  /absolute/new/private-demo
```

The native archive above is for macOS arm64; use the filename produced for your
platform. Installing into `dist/` keeps the native packages beside the compiled
example without changing workspace dependencies.

`local-init` requires a new directory. It creates private attachment and POST
demo buckets, separate uploader and backend principals, exact-origin CORS rules,
an app database, and a private `config.json` containing storage credentials. It
prints the configuration path. Interrupted initialization preserves its data; it
never resets an existing directory.

Create an account by piping its password from a trusted password source to:

```sh
node examples/node/attachments/dist/src/main.mjs account \
  /absolute/new/private-demo/config.json alice
```

Passwords come from standard input and must contain at least 12 characters and
at most 1024 UTF-8 bytes. One trailing newline is removed. Usernames are 3-64
lowercase ASCII letters, digits, dots, underscores, or hyphens, starting with a
letter or digit. There is no public signup route. Each account can have eight
sessions, which expire after eight hours. Login is limited to 20 attempts per
minute across all accounts.

Start the app with cleanup paused:

```sh
node examples/node/attachments/dist/src/main.mjs serve \
  /absolute/new/private-demo/config.json
```

Open `http://127.0.0.1:3000`. Storage listens on `http://127.0.0.1:3001`; both
services bind to loopback. Edit the private configuration to change ports or
origins. Changing the app origin also requires updating bucket CORS through
`putBucketCors`, retaining the methods and headers installed by `local-init`.

## Uploads and cleanup

Attachments accept nonempty UTF-8 text up to 1 MiB. Tabs and newlines are
allowed; binary control characters are rejected. The backend verifies stored
bytes before attaching an object. A filename or declared MIME type is not
verification. Downloads use an attachment disposition.

The browser receives short-lived signed URLs and required headers, never storage
credentials. Each upload signature binds the create-only condition, checksum,
metadata, and Content-Length. The browser supplies Content-Length from the File
body; JavaScript does not set that header.

Attempts expire after five minutes, and each account can have 16 pending
attempts. If a response is lost, use **Check previous upload and attach** to
verify the stored object. An abandoned attempt cannot attach; retry with a fresh
attempt and key. The UI shows the latest 100 attachments.

Enable cleanup only when the app database and object store represent the same
current dataset:

```sh
node examples/node/attachments/dist/src/main.mjs serve \
  /absolute/new/private-demo/config.json \
  --accept-authoritative-state
```

This flag enables cleanup for one process lifetime. It asserts that the app owns
its prefix exclusively, records attempts before issuing URLs, never reuses keys,
and has complete pending and live attachment state. Every start defaults to
paused. After a restore, keep the app isolated and reconcile application and
storage state before enabling deletion; see [backup and
recovery][backup-and-recovery].

Each minute, cleanup handles up to 16 attempts and lists one page of up to 16
keys. It checks pending and attached state before each deletion. Traversals
restart after completion, failure, or process restart, so late uploads remain
discoverable. A completed traversal can compact up to 16 abandoned records; live
ownership survives removal of upload history. A database outage pauses new
deletes, though an already-issued request can still finish.

The app database is bound to a dataset ID, bucket, and prefix. Unsupported
schemas are rejected without resetting data. Never replace the app database
against a live storage prefix to bypass this check.

The backend needs ListBucket permission for the exact configured prefix; the
uploader cannot list. Fresh setup grants this permission. To update an existing
local installation, identify its backend principal through local administration
and run:

```sh
node examples/node/attachments/dist/src/main.mjs local-reconcile-grant \
  /absolute/new/private-demo/config.json BACKEND_PRINCIPAL_ID
```

The command verifies the configured backend key and replaces the app-owned
backend policy. It does not enable cleanup.

## Public sharing

Public sharing is disabled by default. For the app's dedicated local bucket,
stop the app and run:

```sh
node examples/node/attachments/dist/src/main.mjs local-public-grant \
  /absolute/new/private-demo/config.json BACKEND_PRINCIPAL_ID
```

The command verifies the backend key, refuses an unrelated bucket policy, grants
backend tagging, and allows anonymous GET within the configured prefix when the
object has `visibility=public`. It permits public bucket policies while
retaining ACL protections. Use it only when you exclusively administer that
bucket.

Set `publicAttachments: true` in the private configuration and start with
`--accept-authoritative-state`. Users can change visibility only for their own
attached objects. **Refresh status** shows whether a change is pending, applied,
or failed. Disabling the UI option does not unpublish objects; request private
visibility and let reconciliation finish first.

One app process owns the full tag set. It retries bounded batches every minute
and revisits completed changes to repair late writes. Reconciliation cannot
cancel a request already sent to storage or recall cached public bytes. Keep
create-only keys and avoid other tag writers.

## Signed POST demo

The separate `/post.html` page demonstrates reusable signed forms and private
readback. It uses a dedicated bucket, separate from attachments. Fresh setup
adds `postDemo: { bucket: "post-demonstration", prefix: "ephemeral/" }` to the
config. For an existing installation, provision a dedicated private bucket with
exact-origin POST/GET CORS, add this configuration, and run
`local-reconcile-grant` to grant the backend scoped PUT, GET, DELETE, and
exact-prefix listing.

Start with `--accept-authoritative-state` to issue forms and enable cleanup.
This accepts exclusive ownership of the disposable demo prefix, including
deletion of unknown keys. Forms expire after five minutes, accept 0-1 MiB, and
default to status 201; the page also demonstrates 200, 204, and 303. Submitted
fields are sent unchanged, with the file last. Redirects are restricted to the
configured app destination.

The app uses account-scoped keys with a fifteen-minute cleanup deadline. Each
minute it scans up to 16 keys and deletes eligible or unknown objects. Repeated
traversals catch uploads that finish after expiry or an earlier deletion.
Readback can reconcile a lost response; a form can be reused before it expires.
Demo files download as attachments and never enter attachment finalization.

## Remote endpoints and HTTPS

To use an existing compatible endpoint, omit `local` from the configuration and
supply the endpoint, region, uploader credentials, and backend credentials.
Preserve the dataset ID when moving the same data. `init CONFIG` creates a new
app database without opening a local engine and refuses an existing database.
Apply equivalent prefix, tagging, and public-access policies through the remote
service's administration tools.

HTTP origins are accepted only for loopback development. For deployment, follow
the [operations guide][operations-guide]. The app uses its configured origin and
does not trust forwarded headers. Mutating app requests require that Origin,
JSON, and a custom header. Responses disable caching and referrer disclosure.

Bucket CORS must allow the configured origin and required upload/read headers.
Test CORS, FormData, and redirects in the browsers you deploy to; SDK tests do
not exercise browser behavior. The POST demo and attachment upload flow are
separate because attachments require create-only PUT requests.

## Tests

Tests cover accounts, request validation, verified uploads, cleanup, public
visibility, local SDK requests, and coordinated application/store restore. Run
against installed Node.js archives from the repository root:

```sh
release_version=$(python3 -m tools.version show)
python3 -m tools.test_bindings attachments \
  "dist/mikrio-node-$release_version.tgz" \
  --native "dist/mikrio-node-darwin-arm64-$release_version.tgz"
```

Use the archive filenames for your platform. Tests use temporary directories and
loopback listeners. The late-upload test waits for expiry and takes about a
minute. Restore tests stop processes before copying application and storage
state; the unclean variant also checks WAL recovery after process termination.
These tests cover process recovery, not power loss or remote AWS deployment.

[backup-and-recovery]: ../../../docs/operations.md#backup-and-recovery
[operations-guide]: ../../../docs/operations.md#https-and-browser-access
