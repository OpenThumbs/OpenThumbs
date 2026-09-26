//! Simple auth: argon2 passwords and opaque API tokens (stored as sha256).
//! Clients send `Authorization: Bearer <token>`; HTTP Basic with
//! `user:token` or `user:password` is also accepted.

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::{header, request::Parts};
use base64::Engine;
use dataset_meta::{MetaStore, User};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::AppState;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("hash password: {e}"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

pub fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

/// Create a token and return the plaintext (only shown once).
pub async fn issue_token(meta: &MetaStore, user: &User, name: &str) -> anyhow::Result<String> {
    let token = format!("ds_{}", ulid::Ulid::new().to_string().to_lowercase() + &ulid::Ulid::new().to_string().to_lowercase());
    meta.insert_token(&user.id, name, &hash_token(&token)).await?;
    Ok(token)
}

pub const SESSION_COOKIE: &str = "ds_session";
pub const CSRF_HEADER: &str = "x-ds-csrf";

pub fn session_token(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == SESSION_COOKIE)
        .map(|(_, v)| v.to_string())
}

async fn resolve_user(parts: &Parts, meta: &MetaStore) -> Result<Option<User>, AppError> {
    let Some(auth) = parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        // Browser session. Unsafe methods must carry the CSRF header, which a
        // cross-site page cannot set without CORS (not enabled).
        let Some(token) = session_token(parts) else { return Ok(None) };
        let safe = matches!(parts.method, axum::http::Method::GET | axum::http::Method::HEAD);
        if !safe && !parts.headers.contains_key(CSRF_HEADER) {
            return Err(AppError::Forbidden);
        }
        return Ok(meta.user_by_token_hash(&hash_token(&token)).await?);
    };
    if let Some(token) = auth.strip_prefix("Bearer ") {
        return meta.user_by_token_hash(&hash_token(token.trim())).await?.map(Some).ok_or(AppError::Unauthorized);
    }
    if let Some(b64) = auth.strip_prefix("Basic ") {
        let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|_| AppError::Unauthorized)?;
        let creds = String::from_utf8(raw).map_err(|_| AppError::Unauthorized)?;
        let (username, secret) = creds.split_once(':').ok_or(AppError::Unauthorized)?;
        if let Some(u) = meta.user_by_token_hash(&hash_token(secret)).await? {
            if u.username == username {
                return Ok(Some(u));
            }
        }
        if let Some(u) = meta.user_by_name(username).await? {
            if verify_password(secret, &u.password_hash) {
                return Ok(Some(u));
            }
        }
    }
    Err(AppError::Unauthorized)
}

/// Read access: anonymous allowed only when `DS_PUBLIC_READ` is enabled.
pub struct Reader(#[allow(dead_code)] pub Option<User>);

impl FromRequestParts<AppState> for Reader {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let user = resolve_user(parts, &state.meta).await?;
        if user.is_none() && !state.config.public_read {
            return Err(AppError::Unauthorized);
        }
        Ok(Reader(user))
    }
}

/// Write access: any authenticated user.
pub struct Writer(pub User);

impl FromRequestParts<AppState> for Writer {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        resolve_user(parts, &state.meta).await?.map(Writer).ok_or(AppError::Unauthorized)
    }
}

impl AppState {
    pub fn is_admin(&self, user: &User) -> bool {
        self.config.admins.iter().any(|a| a == &user.username)
    }
}
