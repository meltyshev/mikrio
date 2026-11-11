# mikrio

Embedded object storage for Rust, with an optional S3-compatible HTTP listener.

Use the `mikrio::Store` API to open a local managed directory. HTTP support is
enabled by default; disable default features for direct storage access only.
Explicitly close bodies, listeners, and stores while the Tokio runtime remains
alive.

See the [project documentation](https://github.com/meltyshev/mikrio) for
building, deployment, compatibility, and the required shared SQLite runtime.

Licensed under Apache-2.0.
