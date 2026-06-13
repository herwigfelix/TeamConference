use crate::config::TlsConfig;
use rustls::ServerConfig;
use rustls_pemfile::{certs, pkcs8_private_keys};
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

pub fn setup_tls(config: &TlsConfig) -> anyhow::Result<TlsAcceptor> {
    let cert_path = Path::new(&config.cert_file);
    let key_path = Path::new(&config.key_file);

    if config.auto_generate && (!cert_path.exists() || !key_path.exists()) {
        generate_self_signed_cert(&config.cert_file, &config.key_file)?;
    }

    let certs_file = std::fs::File::open(cert_path)
        .map_err(|e| anyhow::anyhow!("Failed to open cert file {:?}: {}", cert_path, e))?;
    let key_file = std::fs::File::open(key_path)
        .map_err(|e| anyhow::anyhow!("Failed to open key file {:?}: {}", key_path, e))?;

    let cert_chain: Vec<_> = certs(&mut BufReader::new(certs_file))
        .filter_map(|r| r.ok())
        .collect();

    let keys: Vec<_> = pkcs8_private_keys(&mut BufReader::new(key_file))
        .filter_map(|r| r.ok())
        .collect();

    let key = keys
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No private key found in key file"))?;

    let tls_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, rustls::pki_types::PrivateKeyDer::Pkcs8(key))
        .map_err(|e| anyhow::anyhow!("TLS config error: {}", e))?;

    Ok(TlsAcceptor::from(Arc::new(tls_config)))
}

fn generate_self_signed_cert(cert_file: &str, key_file: &str) -> anyhow::Result<()> {
    tracing::info!("Generating self-signed TLS certificate...");

    if let Some(parent) = Path::new(cert_file).parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Pure-Rust certificate generation (rcgen) — works on all platforms,
    // no external openssl binary required.
    let certified = rcgen::generate_simple_self_signed(vec![
        "localhost".to_string(),
        "TeamConference Server".to_string(),
    ])
    .map_err(|e| anyhow::anyhow!("Certificate generation failed: {}", e))?;

    std::fs::write(cert_file, certified.cert.pem())?;
    std::fs::write(key_file, certified.key_pair.serialize_pem())?;

    tracing::info!("Self-signed certificate generated: {}, {}", cert_file, key_file);
    Ok(())
}
