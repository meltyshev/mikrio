//! Thin listener translation and completion ownership on NAPI's runtime.

use mikrio_embed::contain;
use napi::bindgen_prelude::{PromiseRaw, Utf16String};
use napi::{Env, Result};
use napi_derive::napi;

use crate::values::try_to_string;
use crate::{NativeCancellation, NativeStore, dispatch, host_error, native_error};

#[napi(object)]
pub struct NativeCredentials {
    pub access_key_id: Utf16String,
    pub secret_access_key: Utf16String,
}

#[napi(object)]
pub struct NativeListenOptions {
    pub host: Option<Utf16String>,
    pub port: Option<f64>,
    pub public_endpoint: Option<Utf16String>,
    pub credentials: Option<NativeCredentials>,
}

#[napi(object, object_from_js = false)]
pub struct NativeServerInfo {
    pub endpoint: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[napi]
pub struct NativeServer {
    server: mikrio::Server,
}

#[napi]
impl NativeStore {
    /// Reserve and bind one listener through the facade.
    ///
    /// # Errors
    /// Rejects invalid options, closed stores and exhausted host admission.
    #[napi(catch_unwind)]
    pub fn listen<'env>(
        &self,
        env: &'env Env,
        input: NativeListenOptions,
        cancellation: &NativeCancellation,
    ) -> Result<PromiseRaw<'env, NativeServer>> {
        let mut options = mikrio::ListenOptions::default();

        if let Some(host) = input.host {
            options = options.host(try_to_string(*env, &host)?);
        }

        if let Some(port) = input.port {
            if !port.is_finite() || port.fract() != 0.0 || !(0.0..=65535.0).contains(&port) {
                return Err(host_error(
                    *env,
                    "InvalidArgument",
                    "port must be an integer between 0 and 65535",
                ));
            }

            // The checked integer envelope is exactly representable by u16.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::as_conversions,
                reason = "finite integral port is checked within u16 before conversion"
            )]
            let port = port as u16;

            options = options.port(port);
        }

        if let Some(origin) = input.public_endpoint {
            options = options.public_endpoint(try_to_string(*env, &origin)?);
        }

        if let Some(credentials) = input.credentials {
            options = options.credentials(
                mikrio::Credentials::new(
                    try_to_string(*env, &credentials.access_key_id)?,
                    try_to_string(*env, &credentials.secret_access_key)?,
                )
                .map_err(|error| native_error(*env, &error))?,
            );
        }

        let store = self.store.clone();

        dispatch(
            env,
            cancellation,
            false,
            async move { store.listen(options).await },
            |_env, server| Ok(NativeServer { server }),
        )
    }
}

#[napi]
impl NativeServer {
    /// Snapshot endpoint, region and credentials without retaining the listener.
    #[napi(getter, catch_unwind)]
    pub fn info(&self) -> NativeServerInfo {
        NativeServerInfo {
            endpoint: self.server.endpoint().to_owned(),
            region: self.server.region().to_owned(),
            access_key_id: self.server.credentials().access_key_id().to_owned(),
            secret_access_key: self.server.credentials().secret_access_key().to_owned(),
        }
    }

    /// Observe listener completion without initiating shutdown.
    ///
    /// # Errors
    /// Reports listener completion or framework dispatch failures.
    #[napi(catch_unwind)]
    pub fn closed<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, ()>> {
        let completion = mikrio::embedding::server_closed(&self.server);

        env.spawn_future_with_callback(
            async move { Ok(contain(completion).await) },
            |env, result| {
                result
                    .map_err(|_| host_error(*env, "IoError", "listener observation panicked"))?
                    .map_err(|error| native_error(*env, &error))
            },
        )
    }

    /// Join listener shutdown independently of ordinary-call admission.
    ///
    /// # Errors
    /// Reports listener shutdown or framework dispatch failures.
    #[napi(catch_unwind)]
    pub fn close<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, ()>> {
        let server = self.server.clone();

        env.spawn_future_with_callback(
            async move { Ok(contain(async move { server.close().await }).await) },
            |env, result| {
                result
                    .map_err(|_| host_error(*env, "IoError", "listener shutdown panicked"))?
                    .map_err(|error| native_error(*env, &error))
            },
        )
    }
}
