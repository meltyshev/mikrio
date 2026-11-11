//! First-rule browser matching is independent of S3 request authorization.

use http::{HeaderMap, HeaderValue, Method, Uri, header};
use mikrio_core::Connection;
use mikrio_core::cors::{CorsRule, header_name};
use mikrio_core::object::BucketName;
use s3s::{Body, HttpRequest, HttpResponse, S3Result, s3_error};

pub(super) struct Cors {
    connection: Connection,
}

impl Cors {
    pub(super) fn new(connection: Connection) -> Self {
        Self { connection }
    }

    pub(super) async fn actual(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
    ) -> HeaderMap {
        let mut response = HeaderMap::new();

        response.insert(
            header::VARY,
            HeaderValue::from_static(if method == Method::OPTIONS {
                "Origin, Access-Control-Request-Method, Access-Control-Request-Headers"
            } else {
                "Origin"
            }),
        );

        if method == Method::OPTIONS || !headers.contains_key(header::ORIGIN) {
            return response;
        }

        let Ok(origin) = origin(headers) else {
            return response;
        };

        let Ok(bucket) = bucket(uri) else {
            return response;
        };

        let Ok(Some(configuration)) = self.connection.cors_for_response(bucket).await else {
            // A missing or unreadable configuration grants no browser access.
            // It must not disclose bucket existence before authentication or
            // replace the storage operation's authorization/error response.
            return response;
        };

        if let Some(rule) = matching(configuration.rules(), origin, method.as_str(), &[]) {
            let mut allowed = HeaderMap::new();

            if project(&mut allowed, rule, origin).is_ok() {
                response.extend(allowed);
            }
        }

        response
    }

    pub(super) async fn preflight(&self, request: HttpRequest) -> S3Result<HttpResponse> {
        let (request, mut body) = request.into_parts();
        let origin = origin(&request.headers)?;

        let method = request
            .headers
            .get(header::ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| s3_error!(InvalidRequest))?;

        let requested = requested_headers(&request.headers)?;
        let bucket = bucket(&request.uri)?;

        body.store_all_limited(0)
            .await
            .map_err(|_| s3_error!(InvalidRequest))?;

        let configuration = self
            .connection
            .cors_for_response(bucket)
            .await
            .map_err(|error| super::storage::error::operation(&error))?
            .ok_or_else(|| s3_error!(AccessDenied))?;

        let rule = matching(configuration.rules(), origin, method, &requested)
            .ok_or_else(|| s3_error!(AccessDenied))?;

        let mut response = HttpResponse::new(Body::empty());
        project(response.headers_mut(), rule, origin)?;

        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_METHODS, value(method)?);

        if !requested.is_empty() {
            response.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                value(&requested.join(", "))?,
            );
        }

        if let Some(seconds) = rule.max_age_seconds {
            response
                .headers_mut()
                .insert(header::ACCESS_CONTROL_MAX_AGE, value(&seconds.to_string())?);
        }

        Ok(response)
    }
}

fn bucket(uri: &Uri) -> S3Result<BucketName> {
    // Use the same decoder and path classifier as the configured path-style
    // S3 service. This does not normalize keys or interpret a forwarded host.
    let path = urlencoding::decode(uri.path()).map_err(|_| s3_error!(InvalidURI))?;
    let path = s3s::path::parse_path_style(&path).map_err(|_| s3_error!(InvalidURI))?;

    let name = path
        .as_bucket()
        .or_else(|| path.as_object().map(|(bucket, _)| bucket))
        .ok_or_else(|| s3_error!(InvalidURI))?;

    BucketName::parse(name).map_err(|_| s3_error!(InvalidBucketName))
}

fn origin(headers: &HeaderMap) -> S3Result<&str> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| s3_error!(InvalidRequest))?;

    if origin.is_empty() || !origin.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(s3_error!(InvalidRequest));
    }

    Ok(origin)
}

fn requested_headers(headers: &HeaderMap) -> S3Result<Vec<String>> {
    let Some(headers) = headers.get(header::ACCESS_CONTROL_REQUEST_HEADERS) else {
        return Ok(vec![]);
    };

    let headers = headers.to_str().map_err(|_| s3_error!(InvalidRequest))?;

    headers
        .split(',')
        .map(|name| {
            let name = name.trim();

            if !header_name(name) || name.contains('*') {
                return Err(s3_error!(InvalidRequest));
            }

            Ok(name.to_ascii_lowercase())
        })
        .collect()
}

fn matching<'a>(
    rules: &'a [CorsRule],
    origin: &str,
    method: &str,
    headers: &[String],
) -> Option<&'a CorsRule> {
    rules.iter().find(|rule| {
        if !rule
            .allowed_origins
            .iter()
            .any(|pattern| pattern_matches(pattern, origin, false))
        {
            return false;
        }

        if !rule.allowed_methods.iter().any(|allowed| allowed == method) {
            return false;
        }

        headers.iter().all(|header| {
            rule.allowed_headers
                .iter()
                .flatten()
                .any(|pattern| pattern_matches(pattern, header, true))
        })
    })
}

fn pattern_matches(pattern: &str, actual: &str, ignore_case: bool) -> bool {
    let equal = |left: &str, right: &str| {
        if ignore_case {
            left.eq_ignore_ascii_case(right)
        } else {
            left == right
        }
    };

    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return equal(pattern, actual);
    };

    if actual.len() < prefix.len() + suffix.len() {
        return false;
    }

    let Some(start) = actual.get(..prefix.len()) else {
        return false;
    };

    let Some(end) = actual.get(actual.len() - suffix.len()..) else {
        return false;
    };

    equal(prefix, start) && equal(suffix, end)
}

fn project(headers: &mut HeaderMap, rule: &CorsRule, origin: &str) -> S3Result<()> {
    let wildcard = rule
        .allowed_origins
        .iter()
        .find(|pattern| pattern_matches(pattern, origin, false))
        .is_some_and(|pattern| pattern == "*");

    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        value(if wildcard { "*" } else { origin })?,
    );

    if !wildcard {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
    }

    if let Some(expose) = &rule.expose_headers
        && !expose.is_empty()
    {
        headers.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            value(&expose.join(", "))?,
        );
    }

    Ok(())
}

fn value(text: &str) -> S3Result<HeaderValue> {
    HeaderValue::from_str(text).map_err(|_| s3_error!(InternalError))
}

#[cfg(test)]
mod tests;
