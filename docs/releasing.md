# Releasing

The [Release workflow](../.github/workflows/release.yaml) validates, builds, and
publishes the distribution archives. Start it through **Actions -> Release ->
Run workflow** from `main`. It calls [Checks](../.github/workflows/checks.yaml)
with the complete release matrix, attests the resulting bundle, creates
`v<version>` for the tested commit, and waits for registry environment
approvals. Maintainers review the source, release notes, platform limits, and
completed artifacts before approving publication of those exact archives.

The version comes from the committed package metadata; the workflow does not
change versions or commit source files. Leave `bootstrap` unchecked for trusted
publishing. Check it when new crates.io or npm packages require temporary secret
tokens.

## Names and prerequisites

The repository is `meltyshev/mikrio`. Before the first release, enable private
vulnerability reporting and verify that the publishing accounts can create or
update these names:

- **crates.io:** `mikrio`, `mikrio-build`, `mikrio-core`, `mikrio-http`,
  `mikrio-embed`, `mikrio-ffi`, and `mikrio-cli`.
- **crates.io, maintained dependencies:** `mikrio-hyper`, `mikrio-hyper-util`,
  `mikrio-s3s`, `mikrio-s3s-multipart`, `mikrio-rusqlite`, and
  `mikrio-libsqlite3-sys`.
- **PyPI:** `mikrio`.
- **npm:** `@mikrio/node`, `@mikrio/node-darwin-arm64`,
  `@mikrio/node-darwin-x64`, `@mikrio/node-linux-arm64-gnu`,
  `@mikrio/node-linux-x64-gnu`, and `@mikrio/node-win32-x64-msvc`.
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
cargo metadata --locked --format-version 1
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

## GitHub repository setup

1. In **Settings -> Actions -> General**, enable Actions and allow the pinned
   actions used by the workflows. The default **Workflow permissions** can stay
   at **Read repository contents and packages permissions**: the workflow grants
   additional permissions only to the jobs that need them. No GitHub personal
   access token or permission to create pull requests is needed.
2. In **Settings -> Environments**, create `crates-io`, `npm`, and `pypi`. For
   each environment, enable **Required reviewers** and add a maintainer. A sole
   maintainer must leave **Prevent self-review** unchecked. Deselect **Allow
   administrators to bypass configured protection rules**.
3. In each environment's **Deployment branches and tags**, choose **Selected
   branches and tags**. Add a rule with ref type **Branch** and name `main`. The
   run remains associated with `main` even after it creates the version tag.
4. In **Settings -> Rules -> Rulesets**, create an active branch ruleset named
   `main-review` targeting `main`. Require a pull request with one maintainer
   approval, squash merging, and the `Checks passed` status from the `Checks`
   workflow. Require branches to be up to date before merging. The aggregate job
   requires every selected audit, test, and distribution check to succeed; a
   failed, canceled, or unexpectedly skipped check cannot pass it. Add
   **Repository administrators** to its bypass list with **Always allow** to
   preserve maintainer direct pushes after local checks. Release runs still
   require their own complete checks before publishing.
5. Create a separate active branch ruleset named `main-history` targeting
   `main`, with **Block force pushes** and **Restrict deletions**, and no bypass
   actors. This keeps history protection independent of maintainer direct
   pushes.
6. Create an active tag ruleset named `release-tags` targeting `v*`. Enable
   **Restrict updates** and **Restrict deletions**. Leave tag creation allowed
   so the workflow's built-in token can create release tags. Keep its bypass
   list empty.
7. In **Settings -> General -> Releases**, select **Enable release
   immutability**. The workflow attaches and verifies all assets while the
   release is a draft, then publishes it last.

See [GitHub environment setup][github-environments], [repository
rulesets][github-rulesets], and [release immutability
settings][github-immutable-releases] for the corresponding GitHub controls.

## Registry setup

New npm and crates.io packages can require temporary API tokens for their first
upload. Select `bootstrap` to use `CARGO_REGISTRY_TOKEN` and `NPM_TOKEN` from
their protected GitHub environments. This checkbox also supports introducing a
new package in a later release. Both registry jobs use secret tokens when it is
selected, so provide credentials for both. Normal runs use trusted publishing
and never fall back to stored tokens.

Use accounts with verified email addresses and two-factor authentication. On
npm, create or obtain publishing access to the `@mikrio` scope. A GitHub
repository does not grant ownership of registry names.

### crates.io

In the publishing account's **Account Settings -> API Tokens**, create a token
with an expiration such as **7 days**. Under **Scopes**, select **publish-new**
and deselect unrelated permissions. Under **Crates -> Add pattern**, add
`mikrio` and `mikrio-*`. If bootstrap must upload new versions of existing
crates, also enable **publish-update**. Add the token in GitHub under **Settings
-> Environments -> crates-io -> Environment secrets** with the name
**`CARGO_REGISTRY_TOKEN`**.

### npm

In npm's **Access Tokens**, generate a granular access token with **Read and
write** package permissions and **Bypass two-factor authentication** enabled for
non-interactive publishing. Allow direct publishing with **`npm publish`** and
select the `@mikrio` scope under **Packages and scopes** so the token can create
and update all six packages, including unpublished names. Give it a short
expiration. Organization administration permissions are not needed. Add it to
the GitHub `npm` environment as **`NPM_TOKEN`**. See [npm token
setup][npm-tokens].

### PyPI

In PyPI's account **Publishing** settings, add a pending GitHub publisher:

| Field             | Value          |
| ----------------- | -------------- |
| PyPI project      | `mikrio`       |
| Owner             | `meltyshev`    |
| Repository        | `mikrio`       |
| Workflow filename | `release.yaml` |
| Environment       | `pypi`         |

It creates the project on the first upload. No PyPI API token is needed. If the
project already belongs to the publishing account, add the same trusted
publisher in the project's settings instead. See [PyPI pending
publishers][pypi-pending-publishers].

## Release

1. Commit the reviewed version change, changelog entry, and supporting changes.
   Run **Actions -> Release -> Run workflow** and select branch `main`. Leave
   `bootstrap` unchecked unless new registry packages require temporary secret
   tokens.
2. Checks audits dependencies, then runs source tests and distribution builds
   concurrently. Each target's native archives are built once and exercised by
   installed consumers. Additional runtime jobs test those same archives. Source
   package consumers validate the Rust archives and a wheel built from the
   Python sdist. Assembly requires every selected check to succeed.
3. Download the `release-<version>` artifact linked in the run summary. It
   contains the packages, `release.json`, `publish-order.json`, reviewed release
   notes, and `SHA256SUMS`. Review test results, dependency audits, licenses,
   and support claims. The workflow attests the bundle and creates `v<version>`
   at the tested commit. Do not create the tag or GitHub Release manually. Tag
   pushes do not start this workflow.
4. In the run's **Review deployments** panel, approve `crates-io`, `npm`, and
   `pypi` when ready. Approvals release the tested archives for publication
   without another build. Registry jobs publish Rust dependencies in order, npm
   native packages before their wrapper, and the complete Python distribution
   set. They do not compile or execute package contents.
5. Separate jobs verify registry hashes and ordinary Rust, Python, and npm
   installations on Linux arm64 and macOS arm64. GitHub release assets are
   uploaded and verified while the release is a draft; the release becomes
   public only after these checks succeed.

The native matrix builds and runs installed Python, Node.js, C/C++, and CLI
checks on macOS arm64/x86_64, Linux arm64/x86_64, and Windows x86_64. Unix
targets also exercise the attachment example and independent Go C consumer.
Linux builds use digest-pinned manylinux 2.28 containers. Additional
installed-package jobs reuse the archives on macOS arm64 and Linux arm64 to
cover Node.js 22/24/26 and Python 3.13/3.14/3.15 before assembly. Wheel auditing
must preserve the native files and shared SQLite identity; an audit that
requires rewriting them fails.

For validation without publication, run **Actions -> Checks -> Run workflow**
with **Build and validate the complete release bundle** selected. This performs
the same validation and assembly, with read-only repository permissions and no
registry credentials, tags, or publication. It is useful when developing build
tooling; a release itself runs all of these checks before requesting approval.

Every release includes fresh Rust, Node.js, and Python dependency audits. These
automated checks do not replace release-specific testing or review of executable
dependencies.

Actions are pinned to reviewed commits, build inputs remain locked, and new
tools and container revisions follow the
[dependency policy](../CONTRIBUTING.md#dependencies). Publishing jobs download
one immutable artifact ID from the same run, reject checksum mismatches, and
have only their required permissions. GitHub artifact attestations cover the
release checksums. npm provenance is requested for both authentication modes;
PyPI uses trusted publishing and upload attestations.

Rust publishing uses the [Cargo registry upload API][cargo-upload] to upload
`.crate` files without repackaging them. Unchanged maintained crate versions are
reused from crates.io before testing and assembly. Any source difference other
than `.cargo_vcs_info.json` requires a version bump.

## Trusted publishing after the first publication

After the first publishing run succeeds, revoke the temporary registry tokens
and delete `CARGO_REGISTRY_TOKEN` and `NPM_TOKEN` from the GitHub environments.
Configure GitHub Actions trusted publishing for all thirteen crates and all six
npm packages before the next release:

| Field             | crates.io      | npm            |
| ----------------- | -------------- | -------------- |
| Owner             | `meltyshev`    | `meltyshev`    |
| Repository        | `mikrio`       | `mikrio`       |
| Workflow filename | `release.yaml` | `release.yaml` |
| Environment       | `crates-io`    | `npm`          |

Use each crate's publishing settings and each npm package's **Settings ->
Trusted publishing**. The workflow filename and environment are case-sensitive;
enter the filename without `.github/workflows/`. On npm, allow direct publishing
with **`npm publish`**; the default stage-only permission is insufficient.

An npm trusted publisher expires if its first successful upload does not happen
within two days. Configure or refresh it shortly before the next release.
Skipping an already-published version does not exercise the trusted publisher.
After enabling trust, select npm's **Require two-factor authentication and
disallow tokens** package setting. crates.io also supports requiring trusted
publishing for future uploads. PyPI converts the pending publisher
automatically.

For subsequent releases, commit the new version and changelog, then run Release
with `bootstrap` unchecked. No local publishing commands, manually created tags,
or stored publishing tokens are required. See [npm trusted
publishers][npm-trusted-publishers] and [crates.io trusted
publishing][crates-io-trusted-publishing].

## Recover a partial publication

Use **Re-run failed jobs** on the same run. Successful build jobs and their
artifact ID must be preserved. Do not rerun all jobs, move a tag, rebuild a
published version, or overwrite an asset. Artifacts are retained for 90 days;
download and preserve the final bundle before beginning publication.

A new publishing run refuses an existing version tag. If credentials or
environment settings were wrong, correct those settings and rerun the original
run's failed jobs. The original run retains its bootstrap selection, commit, and
artifact ID.

Each publisher compares existing registry hashes and uploads only missing files.
A mismatch stops the release. An interrupted upload is not automatically
replayed; the next run first checks whether it committed. PyPI does not use
blanket `skip-existing`, and GitHub uploads do not use `--clobber`. Registry
publication is not atomic: successful uploads remain public if another job
fails. Diagnose the failure and resume with the same bytes before announcing the
release.

npm publication waits up to 20 minutes for each group of accepted uploads to
become visible and match the reviewed checksums. Native packages must pass this
check before the wrapper is uploaded. If registry processing exceeds the wait,
check the package status before resuming. Registry errors include the response
details and `Retry-After` header when present; resolve permission or package
policy errors before retrying.

[cargo-upload]:
  https://doc.rust-lang.org/cargo/reference/registry-web-api.html#publish
[pypi-pending-publishers]:
  https://docs.pypi.org/trusted-publishers/creating-a-project-through-oidc/
[npm-trusted-publishers]: https://docs.npmjs.com/trusted-publishers/
[npm-tokens]: https://docs.npmjs.com/creating-and-viewing-access-tokens/
[crates-io-trusted-publishing]: https://crates.io/docs/trusted-publishing
[github-environments]:
  https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments
[github-rulesets]:
  https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/creating-rulesets-for-a-repository
[github-immutable-releases]:
  https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/establish-provenance-and-integrity/prevent-release-changes
