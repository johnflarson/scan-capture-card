//! TLS for HTTPS and PostgreSQL: rustls with the ring provider, checking
//! certificates against the operating system's store, so no OpenSSL is
//! needed on Linux.

use std::sync::Once;

use anyhow::{Context, Result};
use rustls::ClientConfig;
use rustls_platform_verifier::ConfigVerifierExt;
use tokio_postgres_rustls::MakeRustlsConnect;

/// Make ring the process's rustls provider (reqwest is built without one).
/// Safe to call any number of times.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // Fails only when a provider is already installed, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The TLS connector for PostgreSQL (used when `sslmode` is `prefer` or
/// `require`).
pub fn postgres() -> Result<MakeRustlsConnect> {
    init();
    let config = ClientConfig::with_platform_verifier().context("TLS setup failed")?;
    Ok(MakeRustlsConnect::new(config))
}
