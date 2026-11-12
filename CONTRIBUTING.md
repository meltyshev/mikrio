# Contributing

Use [GitHub issues](https://github.com/meltyshev/mikrio/issues) for bug reports
and proposals. Report vulnerabilities through [private reporting](SECURITY.md).

Read the [build guide](docs/building.md), then run the checks for the code you
change. Include a focused regression test for a behavior change and explain the
problem, resulting behavior, and tests run in the pull request. Keep formatting
changes separate from changes to storage or ownership.

Maintainers may push directly to `main` after running the applicable local
checks. Contributor changes use pull requests with maintainer approval and
passing checks, then squash merge. A failed check on `main` must be fixed or
reverted before release.

## Repository layout

| Path                              | Purpose                              |
| --------------------------------- | ------------------------------------ |
| `crates/mikrio-core`              | Storage, recovery, and maintenance   |
| `crates/mikrio-http`              | Authentication and S3 HTTP           |
| `crates/mikrio`                   | Public Rust API                      |
| `crates/mikrio-embed`             | Foreign-runtime support              |
| `crates/mikrio-{python,node,ffi}` | Native language adapters             |
| `crates/mikrio-cli`               | Command-line interface               |
| `crates/mikrio-build`             | Native-loader build settings         |
| `packages/`                       | Language packages and consumer tests |
| `tests/`                          | Contract, SDK, and embedding tests   |
| `schema/`                         | SQL constraint and migration tests   |
| `tools/`                          | Build and packaging tools            |
| `examples/`                       | Runnable applications                |
| `vendor/`                         | Maintained dependency sources        |

## Implementation

Storage behavior belongs in the core. Adapters translate values, errors,
authorization context, and stream ownership; they do not access engine SQL or
payload paths directly. HTTP requests must never obtain trusted native access.

Preserve transaction, lock, drop, and cancellation order. A canceled caller does
not imply that native work stopped or that a mutation did not commit. Test
failure after side effects as well as rejection before work begins. See
[API ownership](docs/api.md) and [storage](docs/storage.md).

Prefer small, direct implementations and ordinary language conventions. Separate
logical blocks, use early returns for invalid cases, and avoid wrappers that
only rename another operation. Document exported behavior and non-obvious safety
invariants. Comments describe the current behavior.

Use US English for identifiers, comments, documentation, and messages. Keep
comments, Markdown, and other readable files ASCII-only unless non-ASCII text is
required by syntax or data, such as Unicode test fixtures. Preserve exact
external API names, URLs, and third-party license and attribution text.

Use rustfmt and strict Clippy for Rust, Ruff and mypy for Python, and Prettier,
ESLint, and TypeScript for Node.js. Do not suppress a warning across a module to
hide a local problem. Unsafe code needs a nearby explanation of the pointer,
aliasing, ownership, and synchronization assumptions.

Follow `.editorconfig` for indentation and line endings. Keep package identity
and descriptive metadata together, followed by runtime settings and
dependencies. Sort workspace members and dependency names alphabetically. Retain
upstream manifest structure in `vendor/` and let package managers write
lockfiles.

## Dependencies

Keep Cargo, pnpm, and uv lockfiles current and use locked installations. Review
licenses, advisories, sources, build scripts, and executable hooks before
changing the graph. Wait at least 72 hours before adopting a third-party
release; an urgent security exception must identify the exact version, advisory,
and validation. Do not enable arbitrary npm install scripts.

Pin project-owned Rust, JavaScript, and Python dependencies to exact versions,
including build and development tools. Keep runtime support ranges separate from
the exact toolchains used to build and test. Preserve upstream dependency
contracts in maintained source distributions; the committed locks select their
transitive inputs. Do not force incompatible versions with new overrides or
patches. Retain a compatible version and record why an upgrade is deferred.

Run `python3 -m tools.dependencies` before compiling a changed Rust graph. Use
the existing audit configuration for narrowly scoped advisory exceptions; do not
disable audits or introduce package-specific checks in build scripts.

Internal native packages are pinned to the coordinated project version. The
maintained dependencies in `vendor/` retain separate upstream-based versions;
preserve their licenses, patch descriptions, and generated-source checks.

## Releases

Maintainers choose the release version and review release notes. Run the release
workflow from `main` in `rehearsal` mode to build and test, or `publish` mode to
create the version tag and publish after registry environment approval. See the
[release guide](docs/releasing.md).
