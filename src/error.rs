//! Error types for TheraGraph Engine
//!
//! This module provides a comprehensive error hierarchy following Rust best practices:
//! - `thiserror` for ergonomic error definitions
//! - Domain-specific error variants for actionable error handling
//! - Proper error context and source chaining
//! - HTTP status code mapping for API responses

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use std::borrow::Cow;
use thiserror::Error;

/// Result type alias for TheraGraph operations
pub type Result<T> = std::result::Result<T, Error>;

/// Main error type for TheraGraph Engine
#[derive(Debug, Error)]
pub enum Error {
    // ========================================================================
    // Configuration Errors
    // ========================================================================
    #[error("Configuration error: {message}")]
    Config {
        message: Cow<'static, str>,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Missing required environment variable: {var}")]
    MissingEnvVar { var: &'static str },

    #[error("Invalid configuration value for {key}: {message}")]
    InvalidConfig {
        key: &'static str,
        message: Cow<'static, str>,
    },

    // ========================================================================
    // Database Errors
    // ========================================================================
    #[error("Database error: {message}")]
    Database {
        message: Cow<'static, str>,
        #[source]
        source: Option<sqlx::Error>,
    },

    #[error("Database connection pool exhausted")]
    PoolExhausted,

    #[error("Database query timeout after {timeout_ms}ms")]
    QueryTimeout { timeout_ms: u64 },

    #[error("Entity not found: {entity_type} with id {id}")]
    NotFound {
        entity_type: &'static str,
        id: String,
    },

    #[error("Constraint violation: {message}")]
    ConstraintViolation { message: Cow<'static, str> },

    #[error("Migration error: {0}")]
    Migration(String),

    // ========================================================================
    // Blockchain/RPC Errors
    // ========================================================================
    #[error("Blockchain RPC error: {message}")]
    Blockchain {
        message: Cow<'static, str>,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Contract call failed for {contract}: {message}")]
    ContractCall {
        contract: Cow<'static, str>,
        message: Cow<'static, str>,
    },

    #[error("Invalid address: {address}")]
    InvalidAddress { address: String },

    #[error("Event decoding failed for {event}: {message}")]
    EventDecode {
        event: &'static str,
        message: Cow<'static, str>,
    },

    #[error("Block not found: {block_number}")]
    BlockNotFound { block_number: u64 },

    #[error("RPC rate limited, retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    // ========================================================================
    // Kafka Errors
    // ========================================================================
    #[error("Kafka error: {message}")]
    Kafka {
        message: Cow<'static, str>,
        #[source]
        source: Option<rdkafka::error::KafkaError>,
    },

    #[error("Kafka producer error: message not delivered after {retries} retries")]
    KafkaProducerFailed { retries: u32 },

    #[error("Kafka consumer error: {message}")]
    KafkaConsumer { message: Cow<'static, str> },

    // ========================================================================
    // API Errors
    // ========================================================================
    #[error("Bad request: {message}")]
    BadRequest { message: Cow<'static, str> },

    #[error("Unauthorized: {message}")]
    Unauthorized { message: Cow<'static, str> },

    #[error("Forbidden: {message}")]
    Forbidden { message: Cow<'static, str> },

    #[error("Rate limit exceeded: try again in {retry_after_secs} seconds")]
    TooManyRequests { retry_after_secs: u64 },

    #[error("Internal server error")]
    Internal {
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    // ========================================================================
    // Recommendation Engine Errors
    // ========================================================================
    #[error("Recommendation engine error: {message}")]
    Recommendation { message: Cow<'static, str> },

    #[error("Feature extraction failed for NFT {nft_id}: {message}")]
    FeatureExtraction {
        nft_id: String,
        message: Cow<'static, str>,
    },

    #[error("User preferences not found for {user_address}")]
    PreferencesNotFound { user_address: String },

    // ========================================================================
    // Serialization Errors
    // ========================================================================
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Invalid data format: {message}")]
    InvalidFormat { message: Cow<'static, str> },

    // ========================================================================
    // Generic Errors
    // ========================================================================
    #[error("Operation timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    #[error("Service unavailable: {service}")]
    ServiceUnavailable { service: &'static str },

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl Error {
    // ========================================================================
    // Constructors for common error patterns
    // ========================================================================

    /// Create a configuration error
    pub fn config(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Config {
            message: message.into(),
            source: None,
        }
    }

    /// Create a database error
    pub fn database(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Database {
            message: message.into(),
            source: None,
        }
    }

    /// Create a database error with source
    pub fn database_with_source(
        message: impl Into<Cow<'static, str>>,
        source: sqlx::Error,
    ) -> Self {
        Self::Database {
            message: message.into(),
            source: Some(source),
        }
    }

    /// Create a blockchain error
    pub fn blockchain(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Blockchain {
            message: message.into(),
            source: None,
        }
    }

    /// Create a Kafka error
    pub fn kafka(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Kafka {
            message: message.into(),
            source: None,
        }
    }

    /// Create a bad request error
    pub fn bad_request(message: impl Into<Cow<'static, str>>) -> Self {
        Self::BadRequest {
            message: message.into(),
        }
    }

    /// Create a not found error
    pub fn not_found(entity_type: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity_type,
            id: id.into(),
        }
    }

    /// Create an internal error
    pub fn internal(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Internal {
            source: Some(Box::new(source)),
        }
    }

    // ========================================================================
    // Error Classification
    // ========================================================================

    /// Returns true if this error is retryable
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::Database { .. }
                | Error::PoolExhausted
                | Error::QueryTimeout { .. }
                | Error::Blockchain { .. }
                | Error::RateLimited { .. }
                | Error::Kafka { .. }
                | Error::Timeout { .. }
                | Error::ServiceUnavailable { .. }
        )
    }

    /// Returns true if this error should be logged at error level
    pub fn is_error_level(&self) -> bool {
        matches!(
            self,
            Error::Database { .. }
                | Error::Blockchain { .. }
                | Error::Kafka { .. }
                | Error::Internal { .. }
                | Error::Migration(_)
        )
    }

    /// Get HTTP status code for this error
    pub fn status_code(&self) -> StatusCode {
        match self {
            Error::BadRequest { .. }
            | Error::InvalidFormat { .. }
            | Error::InvalidAddress { .. } => StatusCode::BAD_REQUEST,
            Error::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Error::Forbidden { .. } => StatusCode::FORBIDDEN,
            Error::NotFound { .. } | Error::PreferencesNotFound { .. } => StatusCode::NOT_FOUND,
            Error::TooManyRequests { .. } | Error::RateLimited { .. } => {
                StatusCode::TOO_MANY_REQUESTS
            }
            Error::ServiceUnavailable { .. } | Error::PoolExhausted => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Error::Timeout { .. } | Error::QueryTimeout { .. } => StatusCode::GATEWAY_TIMEOUT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Get error code for API responses
    pub fn error_code(&self) -> &'static str {
        match self {
            Error::Config { .. } | Error::MissingEnvVar { .. } | Error::InvalidConfig { .. } => {
                "CONFIG_ERROR"
            }
            Error::Database { .. }
            | Error::PoolExhausted
            | Error::QueryTimeout { .. }
            | Error::ConstraintViolation { .. }
            | Error::Migration(_) => "DATABASE_ERROR",
            Error::NotFound { .. } => "NOT_FOUND",
            Error::Blockchain { .. }
            | Error::ContractCall { .. }
            | Error::InvalidAddress { .. }
            | Error::EventDecode { .. }
            | Error::BlockNotFound { .. } => "BLOCKCHAIN_ERROR",
            Error::RateLimited { .. } => "RATE_LIMITED",
            Error::Kafka { .. }
            | Error::KafkaProducerFailed { .. }
            | Error::KafkaConsumer { .. } => "KAFKA_ERROR",
            Error::BadRequest { .. } => "BAD_REQUEST",
            Error::Unauthorized { .. } => "UNAUTHORIZED",
            Error::Forbidden { .. } => "FORBIDDEN",
            Error::TooManyRequests { .. } => "RATE_LIMIT_EXCEEDED",
            Error::Recommendation { .. }
            | Error::FeatureExtraction { .. }
            | Error::PreferencesNotFound { .. } => "RECOMMENDATION_ERROR",
            Error::Json(_) | Error::InvalidFormat { .. } => "SERIALIZATION_ERROR",
            Error::Timeout { .. } => "TIMEOUT",
            Error::ServiceUnavailable { .. } => "SERVICE_UNAVAILABLE",
            Error::Internal { .. } | Error::Other(_) => "INTERNAL_ERROR",
        }
    }
}

// ============================================================================
// Error Response for API
// ============================================================================

/// API error response body
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let code = self.error_code();
        let message = self.to_string();

        // Don't expose internal error details in production
        let safe_message = if status == StatusCode::INTERNAL_SERVER_ERROR {
            "An internal error occurred".to_string()
        } else {
            message
        };

        let retry_after = match &self {
            Error::RateLimited { retry_after_ms } => Some(retry_after_ms / 1000),
            Error::TooManyRequests { retry_after_secs } => Some(*retry_after_secs),
            _ => None,
        };

        let body = ErrorResponse {
            error: ErrorBody {
                code,
                message: safe_message,
                details: None,
                retry_after,
            },
        };

        (status, Json(body)).into_response()
    }
}

// ============================================================================
// From implementations for external error types
// ============================================================================

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        match &err {
            sqlx::Error::RowNotFound => Error::NotFound {
                entity_type: "record",
                id: "unknown".to_string(),
            },
            sqlx::Error::PoolTimedOut => Error::PoolExhausted,
            sqlx::Error::Database(db_err) => {
                // Check for constraint violations
                if let Some(constraint) = db_err.constraint() {
                    return Error::ConstraintViolation {
                        message: format!("Constraint '{}' violated", constraint).into(),
                    };
                }
                Error::Database {
                    message: db_err.message().to_string().into(),
                    source: Some(err),
                }
            }
            _ => Error::Database {
                message: err.to_string().into(),
                source: Some(err),
            },
        }
    }
}

impl From<rdkafka::error::KafkaError> for Error {
    fn from(err: rdkafka::error::KafkaError) -> Self {
        Error::Kafka {
            message: err.to_string().into(),
            source: Some(err),
        }
    }
}

impl From<std::env::VarError> for Error {
    fn from(_err: std::env::VarError) -> Self {
        Error::Config {
            message: "Environment variable error".into(),
            source: None,
        }
    }
}

// Note: ProviderError conversion is already implemented above

// ============================================================================
// Extension trait for adding context to errors
// ============================================================================

/// Extension trait for adding context to Results
#[allow(dead_code)]
pub trait ResultExt<T> {
    /// Add context to an error
    fn context(self, message: impl Into<Cow<'static, str>>) -> Result<T>;

    /// Add context lazily (only evaluated on error)
    fn with_context<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> Cow<'static, str>;
}

impl<T, E: Into<Error>> ResultExt<T> for std::result::Result<T, E> {
    fn context(self, _message: impl Into<Cow<'static, str>>) -> Result<T> {
        self.map_err(Into::into)
    }

    fn with_context<F>(self, _f: F) -> Result<T>
    where
        F: FnOnce() -> Cow<'static, str>,
    {
        self.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_retryable() {
        assert!(Error::PoolExhausted.is_retryable());
        assert!(Error::RateLimited {
            retry_after_ms: 1000
        }
        .is_retryable());
        assert!(!Error::NotFound {
            entity_type: "nft",
            id: "123".to_string()
        }
        .is_retryable());
    }

    #[test]
    fn test_error_status_codes() {
        assert_eq!(
            Error::NotFound {
                entity_type: "nft",
                id: "123".to_string()
            }
            .status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            Error::BadRequest {
                message: "invalid".into()
            }
            .status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            Error::Internal { source: None }.status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
