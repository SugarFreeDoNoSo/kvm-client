//! Exercises the full TLS client configuration against fresh authenticated connections.
use moonlight_common::{crypto::rustcrypto::RustCryptoBackend, http::pair::PairingCryptoBackend};
use rustls::{
    client::danger::HandshakeSignatureValid,
    pki_types::{CertificateDer, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
    DigitallySignedStruct, DistinguishedName, SignatureScheme,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    thread,
    time::Duration,
};

#[derive(Debug)]
struct PairedClientVerifier(Vec<u8>);

impl ClientCertVerifier for PairedClientVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        if cert.as_ref() != self.0 {
            return Err(rustls::Error::General(
                "Unexpected client certificate".into(),
            ));
        }
        Ok(ClientCertVerified::assertion())
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

#[test]
fn consecutive_sunshine_connections_send_client_certificate_without_resuming() {
    let (certificate, key) = RustCryptoBackend.generate_client_identity().unwrap();
    let certificate = certificate.to_pem().contents().to_vec();
    let key = key.to_pem().contents().to_vec();
    let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_client_cert_verifier(Arc::new(PairedClientVerifier(certificate.clone())))
    .with_single_cert(
        vec![CertificateDer::from(certificate.clone())],
        rustls::pki_types::PrivateKeyDer::try_from(key.clone()).unwrap(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut handshakes = Vec::new();
        for _ in 0..3 {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let connection =
                rustls::ServerConnection::new(Arc::new(server_config.clone())).unwrap();
            let mut tls = rustls::StreamOwned::new(connection, socket);
            let mut request = [0; 2048];
            assert!(tls.read(&mut request).unwrap() > 0);
            assert!(!tls.conn.peer_certificates().unwrap().is_empty());
            handshakes.push(tls.conn.handshake_kind().unwrap());
            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            tls.flush().unwrap();
        }
        handshakes
    });
    let config = crate::net::sunshine_tls_config(certificate.clone(), key, &certificate).unwrap();
    let client = reqwest::blocking::Client::builder()
        .use_preconfigured_tls(config)
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    // Each response closes its connection, matching Sunshine's HTTP behavior.
    for path in ["pair", "serverinfo", "applist"] {
        assert_eq!(
            client
                .get(format!("https://{address}/{path}"))
                .send()
                .unwrap()
                .text()
                .unwrap(),
            "ok"
        );
    }
    assert_eq!(server.join().unwrap(), vec![rustls::HandshakeKind::Full; 3]);
}
