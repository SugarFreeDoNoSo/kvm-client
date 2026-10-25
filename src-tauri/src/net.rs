//! HTTPS and WSS use the same exact-certificate trust policy. No password or
//! cookie is sent by the discovery handshake, and a changed certificate fails
//! the TLS handshake before application data can leave the client.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use reqwest::{
    cookie::{CookieStore, Jar},
    Client,
};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, SignatureScheme,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::Url;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInspection {
    pub address: String,
    pub fingerprint: String,
    pub trusted: bool,
    pub requires_trust: bool,
}

#[derive(Debug)]
struct CertificateVerifier {
    pin: Option<String>,
}

impl ServerCertVerifier for CertificateVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Some(expected) = &self.pin {
            if fingerprint(cert.as_ref()) != *expected {
                return Err(rustls::Error::General(
                    "El certificado del KVM cambió; vuelve a inspeccionar su huella".into(),
                ));
            }
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub fn fingerprint(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn normalize_address(input: &str) -> Result<Url, String> {
    let url = Url::parse(input.trim()).map_err(|_| "Introduce una URL HTTPS válida".to_string())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(
            "Usa solo el origen HTTPS del dispositivo, por ejemplo https://192.168.100.8/".into(),
        );
    }
    Ok(url)
}

pub fn tls_config(pin: Option<String>) -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(CertificateVerifier { pin }))
        .with_no_client_auth();
    Arc::new(config)
}

pub async fn probe_certificate(url: &Url) -> Result<String, String> {
    let host = url.host_str().ok_or("Falta el host")?.to_owned();
    let port = url.port_or_known_default().unwrap_or(443);
    let name = ServerName::try_from(host.clone()).map_err(|e| e.to_string())?;
    let operation = async {
        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| format!("No se pudo conectar al KVM: {e}"))?;
        let tls = TlsConnector::from(tls_config(None))
            .connect(name, tcp)
            .await
            .map_err(|e| format!("No se pudo inspeccionar TLS: {e}"))?;
        let cert = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .ok_or("El KVM no presentó certificado")?;
        Ok(fingerprint(cert.as_ref()))
    };
    tokio::time::timeout(Duration::from_secs(8), operation)
        .await
        .map_err(|_| "Tiempo agotado conectando al KVM".to_string())?
}

#[derive(Default, Serialize, Deserialize)]
struct TrustedDevices {
    devices: BTreeMap<String, String>,
}

fn read_trust(path: &Path) -> Result<TrustedDevices, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("Archivo de confianza inválido: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TrustedDevices::default()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn trusted_pin(path: &Path, address: &str) -> Result<Option<String>, String> {
    Ok(read_trust(path)?.devices.get(address).cloned())
}

pub async fn inspect(path: PathBuf, address: &str) -> Result<DeviceInspection, String> {
    let url = normalize_address(address)?;
    let pin = probe_certificate(&url).await?;
    let trusted = trusted_pin(&path, url.as_str())?.as_ref() == Some(&pin);
    Ok(DeviceInspection {
        address: url.to_string(),
        fingerprint: pin,
        trusted,
        requires_trust: !trusted,
    })
}

pub async fn trust(path: PathBuf, address: &str, expected: &str) -> Result<(), String> {
    let url = normalize_address(address)?;
    let actual = probe_certificate(&url).await?;
    if actual != expected {
        return Err("El certificado cambió desde la inspección. Inspecciona otra vez.".into());
    }
    let mut trusted = read_trust(&path)?;
    trusted.devices.insert(url.to_string(), actual);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(
        &temp,
        serde_json::to_vec_pretty(&trusted).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(temp, path).map_err(|e| e.to_string())
}

pub struct DeviceSession {
    pub address: Url,
    pub tls: Arc<ClientConfig>,
    pub client: Client,
    cookies: Arc<Jar>,
    http_session: String,
}

impl DeviceSession {
    pub async fn login(path: &Path, address: &str, password: &str) -> Result<Self, String> {
        let url = normalize_address(address)?;
        let pin = trusted_pin(path, url.as_str())?
            .ok_or("Primero inspecciona y acepta la huella del KVM")?;
        let tls = tls_config(Some(pin));
        let cookies = Arc::new(Jar::default());
        let client = Client::builder()
            .use_preconfigured_tls((*tls).clone())
            .cookie_provider(cookies.clone())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        let session = Self {
            address: url,
            tls,
            client,
            cookies,
            http_session: uuid::Uuid::new_v4().to_string(),
        };
        let response = session
            .client
            .get(session.endpoint("device")?)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if response.status().is_success() {
            return Ok(session);
        }
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Err(format!("El KVM respondió HTTP {}", response.status()));
        }
        if password.is_empty() {
            return Err("El KVM requiere contraseña".into());
        }
        let login = session
            .client
            .post(session.endpoint("auth/login-local")?)
            .json(&json!({"password":password}))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !login.status().is_success() {
            let status = login.status();
            return Err(match status.as_u16() {
                401 => "Contraseña incorrecta".into(),
                429 => "Demasiados intentos. Espera antes de conectar otra vez.".into(),
                _ => format!("No se pudo autenticar: HTTP {status}"),
            });
        }
        Ok(session)
    }

    pub fn endpoint(&self, path: &str) -> Result<Url, String> {
        self.address.join(path).map_err(|e| e.to_string())
    }

    pub fn signaling_request(
        &self,
    ) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, String> {
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};
        let mut url = self.endpoint("webrtc/signaling/client")?;
        url.set_scheme("wss").map_err(|_| "URL WSS inválida")?;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| e.to_string())?;
        if let Some(cookie) = self.cookies.cookies(&self.address) {
            request.headers_mut().insert(
                "Cookie",
                HeaderValue::from_bytes(cookie.as_bytes()).map_err(|e| e.to_string())?,
            );
        }
        Ok(request)
    }

    #[allow(dead_code)]
    pub async fn query(&self, method: &str) -> Result<Value, String> {
        // Keep this diagnostic interface read-only. HID always uses DataChannel.
        if ![
            "getVideoState",
            "getStreamEncodecType",
            "getVideoRc",
            "getUSBState",
            "ping",
        ]
        .contains(&method)
        {
            return Err("Consulta no permitida".into());
        }
        self.client
            .post(self.endpoint("api/rpc")?)
            .header("X-Session-ID", &self.http_session)
            .json(&json!({"jsonrpc":"2.0","method":method,"params":{},"id":1}))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_validation_rejects_credentials_paths_and_downgrades() {
        for address in [
            "http://192.168.1.1/",
            "https://user:pass@host/",
            "https://host/api",
            "https://host/?token=x",
            "https://host/#foo",
        ] {
            assert!(normalize_address(address).is_err(), "{address}");
        }
        assert_eq!(
            normalize_address(" https://192.168.100.8 ")
                .unwrap()
                .as_str(),
            "https://192.168.100.8/"
        );
    }
    #[test]
    fn wrong_certificate_is_rejected_before_application_data() {
        let verifier = CertificateVerifier {
            pin: Some(fingerprint(b"trusted")),
        };
        let cert = CertificateDer::from(b"different".to_vec());
        assert!(verifier
            .verify_server_cert(
                &cert,
                &[],
                &ServerName::try_from("localhost").unwrap(),
                &[],
                UnixTime::now()
            )
            .is_err());
    }
    #[test]
    fn trust_is_bound_to_origin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        std::fs::write(&path, br#"{"devices":{"https://host/":"AA"}}"#).unwrap();
        assert_eq!(
            trusted_pin(&path, "https://host/").unwrap(),
            Some("AA".into())
        );
        assert_eq!(trusted_pin(&path, "https://other/").unwrap(), None);
    }
}
