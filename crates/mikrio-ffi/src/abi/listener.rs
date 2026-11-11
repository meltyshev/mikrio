//! Listener acquisition and detached snapshots follow the ordinary C allocation rules.

use super::{
    Arc, CloseGate, Error, OptionalU64, ProcessBound, Span, StoreHandle, entry, handles, output,
    required, sized,
};
use crate::handles::{Listener, ServerHandle};

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(super) struct Credentials {
    access_key_id: Span,
    secret_access_key: Span,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ListenOptions {
    struct_size: u64,
    host: Span,
    port: OptionalU64,
    public_endpoint: Span,
    credentials: Credentials,
}

input_layout!(ListenOptions, host, port, public_endpoint, credentials);

pub(super) struct ServerInfo {
    endpoint: String,
    region: String,
    credentials: mikrio::Credentials,
}

#[derive(Default)]
#[repr(C)]
pub(super) struct ServerInfoView {
    endpoint: Span,
    region: Span,
    credentials: Credentials,
}

/// # Safety
/// Follow `include/mikrio.h`: keep handles and declared input bytes live and
/// immutable for this call. Input records and arrays must be aligned; output
/// slots must be aligned, writable and disjoint. Do not race handle release.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_store_listen(
    store: *const StoreHandle,
    options: *const ListenOptions,
    result: *mut *mut ServerHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: sized caller inputs and live handle are borrowed only during this
    // call; both disjoint writable output slots are cleared before work begins.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let options = sized(options)?;

            let mut input = mikrio::ListenOptions::default();

            if let Some(host) = options.host.optional_text()? {
                input = input.host(host);
            }

            if let Some(port) = options.port.get()? {
                input = input.port(
                    u16::try_from(port)
                        .map_err(|_| Error::invalid("port must be between 0 and 65535"))?,
                );
            }

            if let Some(origin) = options.public_endpoint.optional_text()? {
                input = input.public_endpoint(origin);
            }

            match (
                options.credentials.access_key_id.optional_text()?,
                options.credentials.secret_access_key.optional_text()?,
            ) {
                (None, None) => {}
                (Some(access_key_id), Some(secret_access_key)) => {
                    input = input
                        .credentials(mikrio::Credentials::new(access_key_id, secret_access_key)?);
                }
                _ => return Err(Error::invalid("credentials must be supplied together")),
            }

            let owner = required(store)?.0.get()?;
            let store = owner.store.clone();

            let server = handles::wait(
                owner
                    .runtime
                    .submit(async move { store.listen(input).await })?,
            )??;

            *result = Box::into_raw(Box::new(ServerHandle(ProcessBound::new(Listener {
                runtime: Arc::clone(&owner.runtime),
                server,
                close: CloseGate::default(),
            }))));

            Ok(())
        })
    }
}

/// # Safety
/// The handle must remain live and aligned throughout this call, without racing
/// release. Any error slot must be aligned, writable and disjoint from the handle.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_server_close(
    server: *const ServerHandle,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: live immutable handle and disjoint writable error slot.
    unsafe { entry(error, None, || required(server)?.0.get()?.close()) }
}

/// # Safety
/// Keep the aligned handle live and immutable throughout this call, without
/// racing release. Output slots must be aligned, writable and disjoint, as
/// specified in `include/mikrio.h`.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_server_info(
    server: *const ServerHandle,
    result: *mut *mut ServerInfo,
    error: *mut *mut Error,
) -> u32 {
    // SAFETY: handle remains live; result/error slots are disjoint and writable.
    unsafe {
        entry(error, None, || {
            let result = output(result)?;
            let owner = required(server)?.0.get()?;

            *result = Box::into_raw(Box::new(ServerInfo {
                endpoint: owner.server.endpoint().into(),
                region: owner.server.region().into(),
                credentials: owner.server.credentials().clone(),
            }));

            Ok(())
        })
    }
}

/// # Safety
/// A nonnull pointer must identify the matching live, aligned library allocation.
/// Keep it immutable and unreleased until all returned borrowed views expire.
#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn mikrio_v1_server_info_view(
    value: *const ServerInfo,
) -> ServerInfoView {
    // SAFETY: caller retains this detached allocation throughout view use.
    let Some(value) = (unsafe { value.as_ref() }) else {
        return ServerInfoView::default();
    };

    ServerInfoView {
        endpoint: Span::borrowed(&value.endpoint),
        region: Span::borrowed(&value.region),
        credentials: Credentials {
            access_key_id: Span::borrowed(value.credentials.access_key_id()),
            secret_access_key: Span::borrowed(value.credentials.secret_access_key()),
        },
    }
}

release!(mikrio_v1_server_release, ServerHandle);
release!(mikrio_v1_server_info_release, ServerInfo);
