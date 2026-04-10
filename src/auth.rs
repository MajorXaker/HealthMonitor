//! HTTP Basic Auth extractor for axum.
//!
//! This module provides a [`BasicAuth`] extractor that implements
//! [`axum::extract::FromRequestParts`]. When added to a handler's argument list,
//! it automatically validates the `Authorization: Basic <base64>` header against
//! the credentials stored in [`Credentials`] (via axum [`State`]).

use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use thiserror::Error;

/// The valid credentials loaded from the application config.
///
/// This should be inserted into the axum application state so that the
/// [`BasicAuth`] extractor can read it.
#[derive(Debug, Clone)]
pub struct Credentials {
    /// Expected username.
    pub username: String,
    /// Expected password.
    pub password: String,
}

/// Marker struct returned by the [`BasicAuth`] extractor when authentication succeeds.
///
/// Handlers receive this as a parameter to indicate the request is authenticated.
pub struct BasicAuth;

/// Errors that can occur while parsing or validating a Basic Auth header.
#[derive(Debug, Error)]
pub enum AuthError {
    /// The `Authorization` header is missing from the request.
    #[error("missing Authorization header")]
    MissingHeader,
    /// The header value is not valid UTF-8 or does not start with `Basic `.
    #[error("malformed Authorization header")]
    MalformedHeader,
    /// The base64 payload could not be decoded.
    #[error("invalid base64 in Authorization header")]
    InvalidBase64,
    /// The decoded bytes are not valid UTF-8.
    #[error("Authorization credentials are not valid UTF-8")]
    InvalidUtf8,
    /// The decoded string does not contain a `:` separator.
    #[error("Authorization credentials missing colon separator")]
    MissingColon,
    /// The provided credentials do not match the configured credentials.
    #[error("invalid credentials")]
    InvalidCredentials,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        // Always return 401 with a WWW-Authenticate challenge.
        (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                r#"Basic realm="healthmon""#,
            )],
            self.to_string(),
        )
            .into_response()
    }
}

/// Decode and validate Basic Auth credentials from a raw header value.
///
/// The `header_value` should be the full value of the `Authorization` header,
/// e.g. `"Basic dXNlcjpwYXNz"`.
///
/// Returns the decoded `(username, password)` on success.
pub fn decode_basic_auth(header_value: &str) -> Result<(String, String), AuthError> {
    // Strip the "Basic " prefix (case-sensitive per RFC 7617).
    let encoded = header_value
        .strip_prefix("Basic ")
        .ok_or(AuthError::MalformedHeader)?;

    // Decode the base64 payload.
    let decoded_bytes = BASE64
        .decode(encoded.trim())
        .map_err(|_| AuthError::InvalidBase64)?;

    // Interpret as UTF-8.
    let decoded = String::from_utf8(decoded_bytes).map_err(|_| AuthError::InvalidUtf8)?;

    // Split on the first colon to separate username from password.
    let colon_pos = decoded.find(':').ok_or(AuthError::MissingColon)?;
    let username = decoded[..colon_pos].to_string();
    let password = decoded[colon_pos + 1..].to_string();

    Ok((username, password))
}

#[async_trait]
impl<S> FromRequestParts<S> for BasicAuth
where
    S: Send + Sync,
    Credentials: axum::extract::FromRef<S>,
{
    type Rejection = AuthError;

    /// Extract and validate Basic Auth credentials from the incoming request.
    ///
    /// Reads the `Authorization` header, decodes the base64 credentials, and
    /// compares them against the [`Credentials`] in application state.
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // Pull the expected credentials from state.
        let credentials = Credentials::from_ref(state);

        // Get the Authorization header.
        let auth_header = parts
            .headers
            .get(header::AUTHORIZATION)
            .ok_or(AuthError::MissingHeader)?
            .to_str()
            .map_err(|_| AuthError::MalformedHeader)?;

        let (username, password) = decode_basic_auth(auth_header)?;

        // Constant-time-ish comparison (both fields must match).
        if username != credentials.username || password != credentials.password {
            return Err(AuthError::InvalidCredentials);
        }

        Ok(BasicAuth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

    /// A valid `Authorization: Basic <base64>` header should decode correctly.
    #[test]
    fn test_decode_valid_basic_auth() {
        let raw = format!("user123:pass456");
        let encoded = BASE64.encode(raw.as_bytes());
        let header_value = format!("Basic {}", encoded);

        let (username, password) = decode_basic_auth(&header_value).expect("should succeed");
        assert_eq!(username, "user123");
        assert_eq!(password, "pass456");
    }

    /// A header with garbage base64 content should return an error.
    #[test]
    fn test_decode_invalid_base64() {
        // "!!!" is not valid base64
        let header_value = "Basic !!!invalid!!!";
        let result = decode_basic_auth(header_value);
        assert!(
            matches!(result, Err(AuthError::InvalidBase64)),
            "expected InvalidBase64, got {:?}",
            result
        );
    }

    /// A header whose base64 decodes to a string without a `:` should return an error.
    #[test]
    fn test_decode_missing_colon() {
        // Encode a string that has no colon.
        let encoded = BASE64.encode(b"nocolonhere");
        let header_value = format!("Basic {}", encoded);
        let result = decode_basic_auth(&header_value);
        assert!(
            matches!(result, Err(AuthError::MissingColon)),
            "expected MissingColon, got {:?}",
            result
        );
    }

    /// A header without the "Basic " prefix should return a MalformedHeader error.
    #[test]
    fn test_decode_missing_basic_prefix() {
        let result = decode_basic_auth("Bearer sometoken");
        assert!(
            matches!(result, Err(AuthError::MalformedHeader)),
            "expected MalformedHeader, got {:?}",
            result
        );
    }
}
