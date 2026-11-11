//! Authenticated HTTP transport and owned listener over the shared core worker.

mod access;
mod cors;
mod listener;
mod origin;
mod progress;
mod storage;

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use mikrio_core::Connection;
use mikrio_core::access::Bootstrap;
use s3s::config::{S3Config, StaticConfigProvider};
use s3s::service::{S3Service, S3ServiceBuilder};
use tracing::instrument::WithSubscriber;

use access::{Access, REQUEST_KEY};
pub use listener::{Completion, Listener};
use origin::ResponseOrigin;

/// Supported S3 routes over the opened store's authenticated HTTP boundary.
/// The owner of accepted sockets must bound concurrent calls and close them
/// before releasing its store. [`Listener`] supplies that ownership for the facade.
pub struct Service {
    inner: S3Service,
    cors: cors::Cors,
    public_endpoint: Option<ResponseOrigin>,
    connections: tokio::sync::Semaphore,
}

impl Service {
    /// Construct with the already opened store and optional listener-local key.
    /// The region comes exclusively from the recognized store identity.
    ///
    /// # Errors
    /// Refuses a stored region that the selected protocol adapter cannot encode.
    pub fn new(connection: Connection, bootstrap: Option<Arc<Bootstrap>>) -> std::io::Result<Self> {
        let region = connection.info().record.region.clone();
        let access = Access::new(connection.clone(), bootstrap);

        Self::build(
            &region,
            storage::Storage(connection.clone()),
            access,
            connection,
        )
    }

    #[cfg(test)]
    fn with_backend(
        connection: Connection,
        bootstrap: Option<Arc<Bootstrap>>,
        backend: impl s3s::S3,
    ) -> std::io::Result<Self> {
        let region = connection.info().record.region.clone();
        let access = Access::new(connection.clone(), bootstrap).observations();
        Self::build(&region, backend, access, connection)
    }

    /// Serve one accepted HTTP/1 connection with bounded header buffering and
    /// progress-based stalled-peer limits. No listener, thread or executor is made.
    /// Dropping this future closes its socket; queued native access work keeps
    /// its own operation ticket until actual settlement.
    ///
    /// # Errors
    /// Returns transport/timeout failure without request data or credentials.
    pub async fn serve(&self, socket: tokio::net::TcpStream) -> std::io::Result<()> {
        let _slot = self
            .connections
            .try_acquire()
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::WouldBlock))?;

        let origin = self.public_endpoint.clone().map_or_else(
            || ResponseOrigin::parse(&format!("http://{}", socket.local_addr()?)),
            Ok,
        )?;

        let service = hyper::service::service_fn(|request| self.call(request, &origin));
        let mut builder = http1::Builder::new();

        builder
            .max_buf_size(8192)
            .max_headers(100)
            .timer(TokioTimer::new())
            .header_read_timeout(Duration::from_secs(5));

        let connection =
            builder.serve_connection(TokioIo::new(progress::Socket::new(socket)), service);

        // s3s diagnostics include paths, form fields and response bodies. Keep
        // its subscriber scoped to polling this connection, never process-wide.
        // Coarse core diagnostics remain a separate owned integration.
        connection
            .with_subscriber(tracing::subscriber::NoSubscriber::default())
            .await
            .map_err(|_| std::io::Error::other("HTTP connection failed"))
    }

    fn build(
        region: &str,
        backend: impl s3s::S3,
        access: Access,
        connection: Connection,
    ) -> std::io::Result<Self> {
        let mut config = S3Config::default();

        config.expected_region = Some(region.parse().map_err(|_| {
            std::io::Error::other("stored region cannot be encoded by HTTP adapter")
        })?);

        config.sig_v4_allowed_services = vec!["s3".into()];
        config.sig_v4_strict_path = true;
        config.enable_sig_v2 = false;
        config.normalize_forward_slash_path = false;
        config.operation_id_routing = false;
        config.xml_max_body_size = 8 * 1024 * 1024;
        config.form_max_field_size = 20 * 1024;
        config.form_max_fields_size = 20 * 1024;
        config.form_max_envelope_size = Some(20 * 1024);
        config.form_max_parts = 64;
        config.post_object_max_file_size = 5_368_709_120;
        config.aws_chunked_stream_max_chunk_size = 64 * 1024;

        let mut builder = S3ServiceBuilder::new(backend);
        builder.set_config(Arc::new(StaticConfigProvider::new(Arc::new(config))));
        builder.set_auth(access.clone());
        builder.set_access(access);

        Ok(Self {
            inner: builder.build(),
            cors: cors::Cors::new(connection),
            public_endpoint: None,
            connections: tokio::sync::Semaphore::new(64),
        })
    }

    async fn call(
        &self,
        mut request: http::Request<hyper::body::Incoming>,
        origin: &ResponseOrigin,
    ) -> Result<s3s::HttpResponse, s3s::HttpError> {
        let mut random = [0; 16];

        getrandom::fill(&mut random).map_err(|_| {
            s3s::HttpError::from_std_error(Box::new(std::io::Error::other(
                "request identifier unavailable",
            )))
        })?;

        let request_id = s3s::service::RequestId(u128::from_be_bytes(random));
        request.extensions_mut().insert(request_id);

        // Hyper retains repeated headers. Reject singleton ambiguity before a
        // parser can treat a duplicate auth header as absent/anonymous.
        if access::ambiguous_headers(request.headers()) {
            let mut response = http::Response::new(s3s::Body::empty());
            *response.status_mut() = http::StatusCode::BAD_REQUEST;

            response
                .headers_mut()
                .insert("x-amz-request-id", request_id.into());

            response.headers_mut().insert(
                http::header::CONNECTION,
                http::HeaderValue::from_static("close"),
            );

            return Ok(response);
        }

        let cors = self
            .cors
            .actual(request.method(), request.uri(), request.headers())
            .await;

        request
            .extensions_mut()
            .insert(s3s::service::PostObjectOrigin(origin.as_str().to_owned()));

        let request = request.map(progress::incoming);

        let mut response = if request.method() == http::Method::OPTIONS {
            // A browser preflights the actual URL, whose signature covers the
            // intended method and headers, not OPTIONS. Only the subsequent
            // storage request authenticates those credentials.
            let mut response = match self.cors.preflight(request).await {
                Ok(response) => response,
                Err(mut error) => {
                    error.set_request_id(request_id.to_string());

                    error
                        .to_http_response()
                        .map_err(|error| s3s::HttpError::from_std_error(Box::new(error)))?
                }
            };

            response
                .headers_mut()
                .insert("x-amz-request-id", request_id.into());

            response
        } else {
            REQUEST_KEY
                .scope(RefCell::new(None), self.inner.call(request))
                .await?
        };

        for (name, value) in &cors {
            if name == http::header::VARY {
                response.headers_mut().append(name, value.clone());
            } else {
                response.headers_mut().insert(name, value.clone());
            }
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests;
