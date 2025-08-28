use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use rustls::{pki_types::{CertificateDer, PrivateKeyDer, ServerName}};
use rustls::client::danger::{ServerCertVerifier, ServerCertVerified, HandshakeSignatureValid};
use rustls::DigitallySignedStruct;
use rustls::pki_types::UnixTime;
use rustls::SignatureScheme;

pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") { return PathBuf::from(appdata).join("Blit"); }
    }
    // Unix-like default
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("blit");
    }
    PathBuf::from(".blit")
}

fn default_server_cert_paths() -> (PathBuf, PathBuf) {
    let dir = config_dir();
    (dir.join("server-cert.pem"), dir.join("server-key.pem"))
}

pub fn load_or_generate_server_config(cert: Option<PathBuf>, key: Option<PathBuf>) -> Result<rustls::ServerConfig> {
    let (cert_path, key_path) = match (cert, key) {
        (Some(c), Some(k)) => (c, k),
        (None, None) => default_server_cert_paths(),
        _ => return Err(anyhow!("--tls-cert requires --tls-key"))
    };

    if !cert_path.exists() || !key_path.exists() {
        // Generate self-signed and persist for TOFU
        let dir = cert_path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir).ok();
        let cert = rcgen::generate_simple_self_signed(vec!["blitd.local".to_string()])
            .context("generate self-signed cert")?;
        fs::write(&cert_path, cert.serialize_pem().context("serialize cert")?)
            .context("write cert pem")?;
        fs::write(&key_path, cert.serialize_private_key_pem())
            .context("write key pem")?;
    }

    // Load cert and key
    let certs = {
        let mut rd = BufReader::new(fs::File::open(&cert_path).context("open cert")?);
        let mut out = Vec::new();
        for c in rustls_pemfile::certs(&mut rd) {
            let c = c.context("read cert")?;
            out.push(CertificateDer::from(c));
        }
        out
    };
    let key = {
        let mut rd = BufReader::new(fs::File::open(&key_path).context("open key")?);
        let pkcs8: Vec<_> = rustls_pemfile::pkcs8_private_keys(&mut rd).collect();
        if let Some(k) = pkcs8.into_iter().next() { PrivateKeyDer::from(k.context("pkcs8 key")?) }
        else {
            let mut rd2 = BufReader::new(fs::File::open(&key_path).context("reopen key")?);
            let rsa: Vec<_> = rustls_pemfile::rsa_private_keys(&mut rd2).collect();
            let k = rsa.into_iter().next().context("rsa key not found")??;
            PrivateKeyDer::from(k)
        }
    };

    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build server tls config")?;
    Ok(cfg)
}

pub fn known_hosts_path() -> PathBuf { config_dir().join("known_hosts") }

fn read_known_hosts(path: &Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(f) = fs::File::open(path) {
        for line in BufReader::new(f).lines().flatten() {
            if let Some((k,v)) = line.split_once('=') { map.insert(k.trim().to_string(), v.trim().to_string()); }
        }
    }
    map
}

fn write_known_hosts(path: &Path, map: &std::collections::HashMap<String,String>) -> Result<()> {
    if let Some(p) = path.parent() { 
        fs::create_dir_all(p).context("create known_hosts parent dir")?; 
    }
    
    // SECURITY: Atomic write to prevent corruption/races
    let temp_path = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&temp_path).context("create temp known_hosts")?;
        
        // Set secure permissions (0600) immediately
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = f.metadata()?.permissions();
            perms.set_mode(0o600);
            f.set_permissions(perms)?;
        }
        
        // Write format header for future compatibility
        writeln!(f, "# Blit TOFU known_hosts - format version 1")?;
        for (k,v) in map.iter() { 
            writeln!(f, "{}={}", k, v)?; 
        }
        f.flush()?;
        f.sync_all()?; // Force to disk
    }
    
    // Atomic rename over existing file
    fs::rename(&temp_path, path).context("atomic replace known_hosts")?;
    Ok(())
}

fn fp_sha256_hex(cert: &CertificateDer<'_>) -> String {
    let mut h = Sha256::new();
    h.update(cert.as_ref());
    let digest = h.finalize();
    digest.iter().map(|b| format!("{:02x}", b)).collect::<String>()
}

#[derive(Debug)]
struct TofuVerifier {
    hostport: String,
    known_path: PathBuf,
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _dns_name: &ServerName,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let fp = fp_sha256_hex(end_entity);
        let mut map = read_known_hosts(&self.known_path);
        match map.get(&self.hostport) {
            Some(saved) => {
                if saved == &fp {
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(rustls::Error::General("server certificate changed; refusing connection (TOFU)".into()))
                }
            }
            None => {
                map.insert(self.hostport.clone(), fp);
                let _ = write_known_hosts(&self.known_path, &map);
                Ok(ServerCertVerified::assertion())
            }
        }
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PKCS1_SHA256,
        ]
    }
}

pub fn build_client_config_tofu(host: &str, port: u16) -> rustls::ClientConfig {
    let verifier = TofuVerifier { hostport: format!("{}:{}", host, port), known_path: known_hosts_path() };
    rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(verifier))
        .with_no_client_auth()
}

pub fn server_name_for(host: &str) -> ServerName<'static> {
    if let Ok(ip) = host.parse::<IpAddr>() { ServerName::IpAddress(ip.into()) }
    else { ServerName::try_from(host.to_string()).unwrap_or_else(|_| ServerName::try_from("localhost".to_string()).unwrap()) }
}
