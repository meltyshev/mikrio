//! Public listener values and owned shutdown over the shared connection.

use std::sync::Arc;

use mikrio_core::access::{Bootstrap, CredentialClass, Secret};

use crate::{ErrorCause, ErrorCode, StorageError, Store};

/// Detached signing credentials. Debug redacts the secret; snapshots do not
/// guarantee continued authority after a listener closes or a key is revoked.
#[derive(Debug, Clone)]
pub struct Credentials {
    access_key_id: String,
    secret_access_key: Secret,
}

impl Credentials {
    /// Validate a credential pair without provisioning or accessing storage.
    /// Listener options additionally reject the reserved application-key class.
    ///
    /// # Errors
    /// Rejects malformed identifiers and empty/oversized UTF-8 secrets.
    pub fn new(
        access_key_id: impl Into<String>,
        secret_access_key: impl Into<String>,
    ) -> Result<Self, StorageError> {
        let access_key_id = access_key_id.into();
        CredentialClass::parse(&access_key_id).map_err(StorageError::from_identity)?;

        let secret_access_key =
            Secret::new(secret_access_key.into()).map_err(StorageError::from_identity)?;

        Ok(Self {
            access_key_id,
            secret_access_key,
        })
    }

    /// Exact signing identifier.
    #[must_use]
    pub fn access_key_id(&self) -> &str {
        &self.access_key_id
    }

    /// Deliberate secret disclosure for configuring an S3 client or presigner.
    #[must_use]
    pub fn secret_access_key(&self) -> &str {
        self.secret_access_key.expose()
    }
}

/// Side-effect-free listener configuration. Defaults bind loopback port zero
/// and generate fresh bootstrap credentials. TLS belongs to an external proxy.
#[derive(Debug, Clone)]
pub struct ListenOptions {
    host: String,
    port: u16,
    public_endpoint: Option<String>,
    credentials: Option<Credentials>,
}

impl Default for ListenOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 0,
            public_endpoint: None,
            credentials: None,
        }
    }
}

impl ListenOptions {
    /// Bind an IP literal, without DNS lookup. Wildcard binds report loopback.
    #[must_use]
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Bind this port, or let the OS choose one when zero.
    #[must_use]
    pub const fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Set the HTTP(S) origin used for generated object URLs behind a proxy.
    /// It changes neither the bound endpoint nor signing/access decisions.
    #[must_use]
    pub fn public_endpoint(mut self, origin: impl Into<String>) -> Self {
        self.public_endpoint = Some(origin.into());
        self
    }

    /// Supply this listener's bootstrap credentials instead of generating them.
    #[must_use]
    pub fn credentials(mut self, credentials: Credentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Check listener configuration without generating credentials or binding.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for an invalid IP, response origin or supplied
    /// bootstrap credential class. Socket availability is checked at listen.
    pub fn validate(&self) -> Result<(), StorageError> {
        mikrio_http::Listener::validate(&self.host, self.public_endpoint.as_deref())
            .map_err(StorageError::from_core)?;

        if let Some(credentials) = &self.credentials {
            Bootstrap::new(
                credentials.access_key_id.clone(),
                credentials.secret_access_key.clone(),
            )
            .map_err(StorageError::from_identity)?;
        }

        Ok(())
    }
}

/// Owned HTTP listener. Clones alias its close lifetime. Details remain readable
/// after close; dropping the last wrapper requests fallback shutdown.
#[derive(Clone)]
pub struct Server {
    pub(crate) listener: mikrio_http::Listener,
    region: String,
    credentials: Credentials,
}

impl Store {
    /// Start the sole listener on this connection using the caller's Tokio I/O
    /// and time drivers. Returns only after binding and readiness publication.
    ///
    /// # Errors
    /// Rejects invalid options, an occupied port/slot, unavailable runtime
    /// drivers and shutdown that wins startup. Direct access survives failure.
    pub async fn listen(&self, options: ListenOptions) -> Result<Server, StorageError> {
        let bootstrap = match options.credentials {
            Some(credentials) => {
                Bootstrap::new(credentials.access_key_id, credentials.secret_access_key)
                    .map_err(StorageError::from_identity)?
            }
            None => Bootstrap::generate().map_err(|error| StorageError {
                code: ErrorCode::IoError,
                cause: ErrorCause::External(std::io::Error::other(error)),
                mutation_outcome: None,
                partial_result: None,
            })?,
        };

        let credentials = Credentials {
            access_key_id: bootstrap.access_key_id().to_owned(),
            secret_access_key: bootstrap.secret().clone(),
        };

        let listener = mikrio_http::Listener::bind(
            &self.connection,
            &options.host,
            options.port,
            options.public_endpoint.as_deref(),
            Arc::new(bootstrap),
        )
        .await
        .map_err(StorageError::from_core)?;

        Ok(Server {
            listener,
            region: self.info.region.clone(),
            credentials,
        })
    }
}

impl Server {
    /// Observe listener termination without requesting shutdown or owning work.
    ///
    /// # Errors
    /// Preserves a failed listener shutdown or unavailable transport result.
    /// Canceling this wait does not close the listener.
    pub async fn closed(&self) -> Result<(), StorageError> {
        self.listener
            .closed()
            .await
            .map_err(StorageError::from_core)
    }

    /// Actual local HTTP origin, without a bucket path or trailing slash.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        self.listener.endpoint()
    }

    /// Persisted store signing region.
    #[must_use]
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Detached listener-local bootstrap credential snapshot.
    #[must_use]
    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// Stop this listener and settle its HTTP work without closing direct calls.
    /// Concurrent calls join; canceled waits retain actual native protection.
    ///
    /// # Errors
    /// Reports shutdown failure through a borrowed handle. No implicit timeout.
    pub async fn close(&self) -> Result<(), StorageError> {
        self.listener.close().await.map_err(StorageError::from_core)
    }
}
