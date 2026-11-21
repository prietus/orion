//! rustls client configuration shared by all backends (system certificate verifier).

use std::sync::{Arc, OnceLock};

use tokio_rustls::rustls::ClientConfig;

use orion_core::{Error, Result};

pub fn client_config() -> Result<Arc<ClientConfig>> {
    static CONFIG: OnceLock<std::result::Result<Arc<ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            use rustls_platform_verifier::ConfigVerifierExt;
            ClientConfig::with_platform_verifier().map(Arc::new).map_err(|e| e.to_string())
        })
        .clone()
        .map_err(|e| Error::Network(format!("TLS configuration: {e}")))
}
