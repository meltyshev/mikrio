//! Private synchronous C embedding boundary. The canonical caller contract is
//! `include/mikrio.h`; no Rust interface in this crate is supported.
#![deny(missing_docs)]

#[allow(unsafe_code, reason = "checked C pointer and symbol boundary")]
mod abi;
mod diagnostics;
mod error;
mod handles;

#[cfg(test)]
mod test_store;
