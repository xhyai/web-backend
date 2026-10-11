//! Auth extractor for axum handlers.
//!
//! The pure auth logic (token lookup, password hashing) lives in `ksi-auth`;
//! this module wires it into the web layer.

pub use ksi_auth::{lookup_token, new_token, verify_password, AuthRequest, AuthResponse, AuthUser};
