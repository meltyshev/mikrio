# mikrio-ffi

C interface for mikrio object storage.

The C header is [`include/mikrio.h`][include-mikrio-h]. Platform archives
include the ABI-versioned library, linker alias or import library, and matching
SQLite companion.

See the [project documentation](https://github.com/meltyshev/mikrio) for
building, deployment, compatibility, and the required shared SQLite runtime.

Licensed under Apache-2.0.

[include-mikrio-h]:
  https://github.com/meltyshev/mikrio/blob/main/crates/mikrio-ffi/include/mikrio.h
