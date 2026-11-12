# Building and testing

Run commands from the repository root. Use the Rust toolchain in
`rust-toolchain.toml`, Python from `.python-version`, Node.js 26.13.0 for
contributor tooling, pnpm 12.18.0, and uv 0.13.7. Python build and development
dependencies are locked in `packages/python/uv.lock`. A C compiler and the
platform SDK are required for native builds. End users of wheels and native npm
packages do not need Rust.

CI runs dependency checks on each change, release rehearsal, and publication,
and weekly on `main`. These use cargo-deny 0.20.2 for Rust advisories, licenses,
and sources, pnpm audit for both JavaScript lockfiles, and the locked pip-audit
version for the Python contributor and SDK graphs. Audit failures block release
assembly; dependency upgrades and exceptions still require maintainer review.

Run `python3 -m tools.dependencies` before compiling a changed Rust graph. It
verifies registry checksums, release ages, and yanks for both Cargo lockfiles.
pnpm and uv enforce their configured release-age policies. CI uses frozen or
locked installs with dependency scripts and Python source builds disabled.

Enable Renovate for the repository using `.github/renovate.json`. It groups
routine update proposals weekly with the same three-day delay and no automatic
merging. Security fixes are proposed separately for maintainer review.

## Setup

```sh
rustup show
python3 -m tools.version check
python3 -m tools.dependencies
pnpm install --frozen-lockfile
uv sync --project packages/python --locked --group build --no-install-project --no-build
```

Use `--offline` with Cargo, pnpm, and uv only after the corresponding locked
inputs are cached. The workspace disables npm lifecycle scripts. Native npm
packages are intentionally excluded from source-workspace dependency resolution;
the installed-package tests select locally built archives explicitly.

## Rust and schema tests

```sh
cargo fmt -- --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo clippy -p mikrio --no-default-features --locked \
  --all-targets -- -D warnings
cargo test --workspace --locked -- --test-threads=1
cargo test -p mikrio --no-default-features --locked -- --test-threads=1
RUSTDOCFLAGS="-D warnings" cargo doc -p mikrio --locked --no-deps
RUSTDOCFLAGS="-D warnings" cargo doc -p mikrio \
  --no-default-features --locked --no-deps
python3 schema/verify.py
python3 -m unittest discover -s tools/tests
```

Set `PYO3_PYTHON` to the Python interpreter used for native binding builds when
it is not the default interpreter. On Unix, extension-module builds may also
need `PYO3_BUILD_EXTENSION_MODULE=1`; the Python extension is exercised through
its installed wheel rather than the default Rust test target.

Tests use temporary stores, child processes, and loopback sockets. Run them in
an environment that permits those operations. Deliberately ignored Rust entries
include subprocess fixture servers and large or resource-constrained workloads;
they are not ordinary standalone tests. The default test command does not cover
power loss, full-capacity workloads, or every supported platform.

`CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0` reduce development disk
use. Release builds are a separate validation configuration. Parser fuzz targets
and seed inputs live in `fuzz/`; ordinary CI does not run fuzzing.

## Python, TypeScript, and Markdown checks

Using the Python environment created above:

```sh
uv run --project packages/python --no-sync python -m ruff check \
  tools schema packages/python tests
uv run --project packages/python --no-sync python -m ruff format --check \
  tools schema packages/python tests
uv run --project packages/python --no-sync python -m mypy --strict tools schema
node node_modules/typescript-compiler/bin/tsc -p packages/node/tsconfig.json
node node_modules/typescript-compiler/bin/tsc \
  -p packages/node/tsconfig.test.json --noEmit
node node_modules/typescript-compiler/bin/tsc \
  -p examples/node/attachments/tsconfig.json --noEmit
pnpm exec eslint \
  packages/node/src packages/node/tests \
  examples/node/attachments/src examples/node/attachments/tests \
  tests/sdk/node/src --max-warnings 0
pnpm exec prettier --check \
  '.github/workflows/*.yaml' .github/renovate.json .prettierrc.json \
  package.json pnpm-workspace.yaml eslint.config.mjs \
  'packages/*/*.json' 'examples/**/package.json' 'examples/**/tsconfig.json' \
  'tests/sdk/node/*.json' tests/sdk/node/pnpm-workspace.yaml \
  packages/node/src packages/node/tests \
  examples/node/attachments/src examples/node/attachments/tests \
  tests/sdk/node/src
pnpm exec prettier --check \
  '*.md' 'docs/**/*.md' 'crates/*/README.md' 'packages/*/README.md' \
  'examples/**/README.md' 'vendor/**/*.md'
```

TypeScript 7 compiles the packages. The separate TypeScript 6 dependency
supports the selected typed ESLint parser; the compiler executable is named
explicitly to avoid selecting the parser's compiler version by accident.

## Native distributions

The build tool stages real Cargo outputs and their matching SQLite companion,
then checks native archive contents. It does not publish anything.

```sh
python3 -m tools.build cli
python3 -m tools.build ffi
python3 -m tools.build node
uv run --project packages/python --no-sync python -m tools.build python
uv run --project packages/python --no-sync python -m tools.build sdist
python3 -m tools.build rust
```

Outputs go to `dist/`; `--output` selects another directory. `--target` accepts
the five triples in the [support guide](../SUPPORT.md#platforms). The
appropriate linker, SDK, and target toolchain must be installed; specifying a
target does not provision a cross-compiler or establish native execution.
Windows C packaging runs in the MSVC developer environment to generate the
import library for `mikrio.dll`.

Linux wheels for PyPI must be built and checked against an actual manylinux
baseline. Pass its validated tag with `--compatibility`; do not relabel an
arbitrary Linux build to imply an older glibc floor. Keep the shared SQLite
loader identity when inspecting or repairing native dependencies.

The Node.js build requires the contributor dependencies and invokes `npm pack`
with scripts disabled. The Python build invokes the locked maturin version. Rust
source packaging orders maintained dependencies before their consumers. Test the
archives with an external consumer before publishing, then verify installation
from the registry.

`tools.build_release` composes these builders and the installed-consumer tests
used by the release workflow. Use `native --target macos-arm64` (or another
native target name from the support guide) for CLI, C, Node.js, and Python
archives, or `sources` for Rust archives and the Python sdist. Select an empty
directory with `--output`; these commands build and test without publishing.

Verify the maintained S3 templates with
`python3 -m tools.regenerate_s3s --check`. Omit `--check` to regenerate the
patched operations and COPY result declarations from
`vendor/s3s-0.17.0/codegen/` after changing their templates.

## Installed consumers

```sh
release_version=$(python3 -m tools.version show)
uv run --project packages/python --no-sync python -m tools.test_bindings \
  python "dist/mikrio-$release_version-cp312-abi3-macosx_11_0_arm64.whl"
python3 -m tools.test_bindings node \
  "dist/mikrio-$release_version.tgz" \
  --native "dist/mikrio-node-darwin-arm64-$release_version.tgz"
python3 -m tools.test_bindings attachments \
  "dist/mikrio-$release_version.tgz" \
  --native "dist/mikrio-node-darwin-arm64-$release_version.tgz"
python3 -m tools.test_ffi "dist/mikrio-c-$release_version-macos-arm64.tar.gz"
python3 -m tools.test_rust dist
```

Use the actual filenames produced for the selected target. These commands
install local archives into an isolated temporary consumer and run the ordinary
package tests outside the checkout. `--test` selects a specific test file. Add
`--probes` to the Python or Node.js command to compile and exercise the stream
and diagnostic test extensions. Those extensions are test-only artifacts.

The Rust consumer extracts the source archives and substitutes them for the
unpublished registry names. It verifies the selected third-party dependencies
against `Cargo.lock` before compilation, builds with `--locked`, then runs a
relocated executable with its SQLite companion. Repeat registry verification
after the maintained dependencies are published.

## SDK tests

Install the independently locked SDK inputs, then run the owned fixture:

```sh
uv sync --project tests/sdk/python --locked --no-install-project --no-build
pnpm --dir tests/sdk/node install --frozen-lockfile
uv run --project tests/sdk/python --no-sync python -m tools.test_sdk storage
uv run --project tests/sdk/python --no-sync python -m tools.test_sdk integrity
uv run --project tests/sdk/python --no-sync python -m tools.test_sdk phases
uv run --project tests/sdk/python --no-sync python -m tools.test_sdk \
  listener --release --suite multipart --suite post --suite transfers
```

The runner builds the fixture, starts an owned loopback server, executes Python
and JavaScript clients, and checks that the server shuts down cleanly. Fixture
credentials are synthetic and temporary. These tests do not contact AWS.

The SDK transport and response-loss fixtures have independent unit tests:

```sh
for suite in fixtures http_loss request_defaults transfer_sources; do
  uv run --project tests/sdk/python --no-sync python -m unittest discover \
    -s tests/sdk -p "test_$suite.py"
done
node --test \
  tests/sdk/node/dist/reporter.test.mjs \
  tests/sdk/node/dist/request-defaults.test.mjs \
  tests/sdk/node/dist/transfer-sources.test.mjs
```

With Go 1.27.3 installed, add `--go go` to the listener command for the locked
Go SDK suite, or to `tools.test_ffi` for the independent cgo consumer. Go
integration tests supplement the supported language packages; mikrio has no Go
registry package. The SDK module uses `go.mod` and `go.sum` with read-only
resolution. CI runs both Go integrations with the exact module toolchain and
`GOTOOLCHAIN=local`, which prevents automatic toolchain replacement.

## Deploying Rust applications

Cargo's build output includes `libmikrio_sqlite3.dylib`, `libmikrio_sqlite3.so`,
or `mikrio_sqlite3.dll`. An executable deployed outside Cargo must retain that
companion. On Unix, give the application a relative loader path in `build.rs`:

```rust
fn main() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path"),
        Ok("linux") => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN"),
        _ => {}
    }
}
```

Stage a compiled executable with the matching companion using
`python3 -m tools.native_runtime PATH_TO_EXECUTABLE OUTPUT_EXECUTABLE`. Verify
the staged application after moving it outside the build tree and without loader
environment overrides.
