# Releasing

The [release workflow](../.github/workflows/release.yaml) builds, tests, and
publishes the distribution archives. A manual run from `main` is a build-only
rehearsal. Pushing a matching `v<version>` tag starts a release; registry
uploads wait for approval of the protected environments. Maintainers review the
source, release notes, platform limits, and completed artifacts before
approving.

## Names and prerequisites

The repository is `meltyshev/mikrio`. Before the first release, enable private
vulnerability reporting and establish registry ownership for these names:

- **crates.io:** `mikrio`, `mikrio-build`, `mikrio-core`, `mikrio-http`,
  `mikrio-embed`, `mikrio-ffi`, and `mikrio-cli`.
- **crates.io, maintained dependencies:** `mikrio-hyper`, `mikrio-hyper-util`,
  `mikrio-s3s`, `mikrio-s3s-multipart`, `mikrio-rusqlite`, and
  `mikrio-libsqlite3-sys`.
- **PyPI:** `mikrio`.
- **npm:** `mikrio`, `@mikrio/node-darwin-arm64`, `@mikrio/node-darwin-x64`,
  `@mikrio/node-linux-arm64-gnu`, `@mikrio/node-linux-x64-gnu`, and
  `@mikrio/node-win32-x64-msvc`.
- **GitHub Releases:** CLI and C archives, checksums, and release notes.

Workspace packages, Python, the Node.js wrapper, and native npm packages use the
same version. Coupled internal dependencies select that exact release.
Maintained dependencies keep their upstream-based versions. C ABI major 1 and
storage format 1 do not change just because the package version changes.

## Updating the version

`workspace.package.version` in the root `Cargo.toml` is authoritative. Rust
packages and Python metadata inherit it; native npm manifests and CLI/C archive
names derive from it during the build. Update the release metadata with:

```sh
python3 -m tools.version set 0.1.1
python3 -m tools.version check
python3 -m tools.version show
```

Replace `0.1.1` with the intended `MAJOR.MINOR.PATCH` release. The command
updates first-party versions and exact dependency pins without upgrading
dependencies. Prerelease and build-metadata suffixes are not supported. Python
inherits the Cargo version; the uv and pnpm locks need no version-only rewrite.

Verify that the locks remain valid:

```sh
cargo metadata --locked --offline --format-version 1
uv lock --project packages/python --check
pnpm install --frozen-lockfile
```

Review the diff, add a [changelog entry](../CHANGELOG.md), and update support
and compatibility claims. Leave historical entries, test-fixture versions,
private example versions, and maintained dependency versions unchanged unless
those components also changed. Rebuild affected archives and notices.

## Build and inspect

Use a clean, reviewed source commit and locked inputs. Run the checks in the
[build guide](building.md), including installed-consumer tests and dependency
audits. Record tested platforms and known limitations in the release notes.

Build the source crates, Python sdist and wheels, Node.js wrapper and five
native packages, and CLI/C archives for the five distribution targets. The
complete target list is in the [support guide](../SUPPORT.md). All native
distributions include the matching shared SQLite runtime and third-party
notices. npm packages and CLI/C archives also include a README with usage and
deployment guidance.

Inspect the archives, not only their source manifests. Verify names, versions,
declarations, licenses, companion identity, native architecture, supported wheel
tags, and the absence of unrelated files. Install outside the checkout and
exercise relocated binaries without compiler or loader-path fallback. Check the
Rust package graph from an external consumer as well as the workspace.

`dist/SHA256SUMS` records the generated artifact hashes. Keep the final tested
files immutable. Repository or metadata changes after the build require affected
artifacts to be rebuilt and checked before publication.

## Configure publishing

Create GitHub environments `crates-io`, `npm`, and `pypi`. Restrict deployment
to selected **tags** matching `v*`, require a maintainer review, and disable
bypass of the protection rules. A sole maintainer must allow self-review.
Protect `main` and release tags against unauthorized changes, deletion, and
force pushes. Enable GitHub's immutable releases before publication: the
workflow attaches all assets to a draft and publishes it last.

Require the `Checks` workflow's `Audit dependencies`,
`Check and test (ubuntu-24.04-arm)`, and `Check and test (macos-15)` jobs on
`main`. These are the job names to select in branch protection or a ruleset.

All trusted publishers use owner `meltyshev`, repository `mikrio`, and workflow
filename **`release.yaml`**. The filename and environment are case-sensitive.

- **PyPI (`pypi`):** Add a pending publisher for `mikrio`; it creates the
  project on the first upload.
- **npm (`npm`):** Publish the six packages with a maintainer account, then
  configure a GitHub Actions trusted publisher in each package's settings. Allow
  direct publishing with `npm publish`; the default stage-only permission does
  not authorize this workflow.
- **crates.io (`crates-io`):** Publish the thirteen crates with a narrowly
  scoped, short-lived token, then configure trusted publishing for each crate.

Use account two-factor authentication. After the initial npm upload, select the
package setting that requires 2FA and disallows traditional publishing tokens;
trusted publishing remains available. Configure npm trust shortly before its
first automated upload: a new configuration expires if unused for two days. No
long-lived npm, PyPI, or Cargo token belongs in GitHub secrets for this
workflow.

Official setup instructions: [PyPI pending publishers][pypi-pending-publishers],
[npm trusted publishers][npm-trusted-publishers], [crates.io trusted
publishing][crates-io-trusted-publishing], and [GitHub immutable
releases][github-immutable-releases].

## Rehearse and release

1. Commit the reviewed version change, changelog entry, and supporting changes.
   Run **Release -> Run workflow** from `main` for a build-only rehearsal.
2. Inspect the completed run and download its `release-<version>` artifact. It
   contains the packages, `release.json`, `publish-order.json`, reviewed release
   notes, and `SHA256SUMS`. Review test results, dependency audits, licenses,
   and support claims.
3. Create and push `v<version>` pointing to the reviewed commit on `main` when
   ready to publish. Tag runs repeat the checks and builds. Review the **tag
   run's** artifact before approving the three registry environments; that
   artifact, rather than an earlier rehearsal, is the publication input.
4. The workflow publishes Rust dependencies in order, npm native packages before
   their wrapper, and the complete Python distribution set. Registry jobs use
   short-lived OIDC credentials and do not compile or execute package contents.
5. Separate jobs verify registry hashes and ordinary Rust, Python, and npm
   installations on Linux arm64 and macOS arm64. GitHub release assets are
   uploaded and verified while the release is a draft; the release becomes
   public only after these checks succeed.

The native matrix builds and runs installed Python, Node.js, C/C++, and CLI
checks on macOS arm64/x86_64, Linux arm64/x86_64, and Windows x86_64. Linux
builds use digest-pinned manylinux 2.28 containers. Additional installed-package
jobs reuse those archives on macOS arm64 and Linux arm64 to cover Node.js
22/24/26 and Python 3.13/3.14/3.15 before assembly. Wheel auditing must preserve
the native files and shared SQLite identity; an audit that requires rewriting
them fails. The sdist is independently built into a wheel and tested. Existing
repository checks run through [checks.yaml](../.github/workflows/checks.yaml),
including fresh Rust, Node.js, and Python dependency audits. These automated
checks do not replace release-specific testing or review of executable
dependencies.

Actions are pinned to reviewed commits, build inputs remain locked, and new
tools and container revisions follow the
[dependency policy](../CONTRIBUTING.md#dependencies). Publishing jobs download
one immutable artifact ID from the same run, reject checksum mismatches, and
have only their required permissions. GitHub artifact attestations cover the
release checksums; npm and PyPI trusted publishing provide registry provenance.

Rust publishing uses the [Cargo registry upload API][cargo-upload] to upload
`.crate` files without repackaging them. Unchanged maintained crate versions are
reused from crates.io before testing and assembly. Any source difference other
than `.cargo_vcs_info.json` requires a version bump.

## First npm and crates.io publication

For the first release, let the tag run reach its environment approvals. Download
its final artifact, extract it outside the checkout or under `dist/`, and use a
clean checkout of the exact commit recorded in `release.json`. Verify it with:

```sh
python3 -m tools.release verify /path/to/release
```

After reviewing that bundle, log into npm interactively with a maintainer
account that can publish `mikrio` and the `@mikrio` scope, then run:

```sh
python3 -m tools.publish npm /path/to/release
```

For crates.io, create an expiring token with the permissions needed to create
the listed crates. Supply it through `CARGO_REGISTRY_TOKEN` in the local process
environment, without writing it into a file or command history, then run:

```sh
python3 -m tools.publish crates /path/to/release
```

Revoke that token immediately afterward. Configure trusted publishing for every
new crate, then approve the waiting GitHub jobs. Already-uploaded versions are
accepted only when their hashes match the bundle. Skipping an existing npm
version does not exercise its trusted publisher: configure or refresh npm trust
within two days before the first automated upload of a new version. PyPI uses
its pending publisher directly and needs no manual token. Initial manual npm
uploads do not acquire GitHub OIDC provenance retroactively; subsequent
automated versions do. The release's GitHub artifact attestations still identify
the bootstrap files.

## Recover a partial publication

Use **Re-run failed jobs** on the same run. Successful build jobs and their
artifact ID must be preserved. Do not rerun all jobs, move a tag, rebuild a
published version, or overwrite an asset. Artifacts are retained for 90 days;
download and preserve the final bundle before beginning publication.

Each publisher compares existing registry hashes and uploads only missing files.
A mismatch stops the release. An interrupted upload is not automatically
replayed; the next run first checks whether it committed. PyPI does not use
blanket `skip-existing`, and GitHub uploads do not use `--clobber`. Registry
publication is not atomic: successful uploads remain public if another job
fails. Diagnose the failure and resume with the same bytes before announcing the
release.

[cargo-upload]:
  https://doc.rust-lang.org/cargo/reference/registry-web-api.html#publish
[pypi-pending-publishers]:
  https://docs.pypi.org/trusted-publishers/creating-a-project-through-oidc/
[npm-trusted-publishers]: https://docs.npmjs.com/trusted-publishers/
[crates-io-trusted-publishing]: https://crates.io/docs/trusted-publishing
[github-immutable-releases]:
  https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases
