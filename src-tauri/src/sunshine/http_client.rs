//! HTTP transport with mutual TLS and exact certificate pinning after pairing.
use moonlight_common::{
    error::Error,
    http::{
        client::blocking_client::RequestClient, ClientInfo, Endpoint, QueryBuilder,
        QueryBuilderError, QueryParam, Request, TextResponse,
    },
};
use pem::Pem;
use std::{error::Error as StdError, str::FromStr, time::Duration};

#[derive(Clone)]
pub struct PinnedClient(reqwest::blocking::Client);

struct EncodedQuery<'a>(&'a mut url::Url);

impl QueryBuilder for EncodedQuery<'_> {
    fn append(&mut self, param: QueryParam<'_>) -> Result<(), QueryBuilderError> {
        self.0.query_pairs_mut().append_pair(param.key, param.value);
        Ok(())
    }
}

fn build_url<E: Endpoint>(
    https: bool,
    info: ClientInfo,
    host: &str,
    request: &E::Request,
) -> Result<url::Url, Error> {
    let scheme = if https { "https" } else { "http" };
    let mut url = url::Url::parse(&format!("{scheme}://{host}{}", E::path())).map_err(error)?;
    let mut query = EncodedQuery(&mut url);
    info.append_query_params(&mut query).map_err(error)?;
    request.append_query_params(&mut query).map_err(error)?;
    Ok(url)
}
fn error(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Other(Box::new(e))
}

fn request_error(url: &url::Url, e: reqwest::Error) -> Error {
    let mut target = url.clone();
    target.set_query(None);
    target.set_fragment(None);
    let e = e.without_url();
    let mut message = format!(
        "Solicitud {} a {target}: {e}",
        url.scheme().to_ascii_uppercase()
    );
    let mut cause = e.source();
    while let Some(source) = cause {
        message.push_str(": ");
        message.push_str(&source.to_string());
        cause = source.source();
    }
    error(std::io::Error::other(message))
}
impl PinnedClient {
    fn new(timeout: Duration, tls: Option<rustls::ClientConfig>) -> Result<Self, Error> {
        let mut builder = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if let Some(tls) = tls {
            builder = builder.use_preconfigured_tls(tls);
        }
        Ok(Self(builder.build().map_err(error)?))
    }
    fn text<E: Endpoint>(
        &self,
        https: bool,
        info: ClientInfo,
        host: &str,
        request: &E::Request,
    ) -> Result<E::Response, Error>
    where
        E::Response: TextResponse,
    {
        let url = build_url::<E>(https, info, host, request)?;
        let response = self
            .0
            .get(url.to_string())
            .send()
            .map_err(|e| request_error(&url, e))?
            .error_for_status()
            .map_err(|e| request_error(&url, e))?;
        let response = response.text().map_err(|e| request_error(&url, e))?;
        E::Response::from_str(&response)
    }
}
impl RequestClient for PinnedClient {
    fn with_defaults() -> Result<Self, Error> {
        Self::new(Duration::from_secs(10), None)
    }
    fn with_defaults_long_timeout() -> Result<Self, Error> {
        Self::new(Duration::from_secs(90), None)
    }
    fn with_certificates(key: &Pem, certificate: &Pem, server: &Pem) -> Result<Self, Error> {
        let tls = crate::net::sunshine_tls_config(
            certificate.contents().to_vec(),
            key.contents().to_vec(),
            server.contents(),
        )
        .map_err(|e| error(std::io::Error::other(e)))?;
        Self::new(Duration::from_secs(10), Some(tls))
    }
    fn send_http<E>(
        &self,
        info: ClientInfo,
        host: &str,
        request: &E::Request,
    ) -> Result<E::Response, Error>
    where
        E: Endpoint,
        E::Response: TextResponse,
    {
        self.text::<E>(false, info, host, request)
    }
    fn send_https<E>(
        &self,
        info: ClientInfo,
        host: &str,
        request: &E::Request,
    ) -> Result<E::Response, Error>
    where
        E: Endpoint,
        E::Response: TextResponse,
    {
        self.text::<E>(true, info, host, request)
    }
    fn send_https_with_bytes<E>(
        &self,
        info: ClientInfo,
        host: &str,
        request: &E::Request,
    ) -> Result<Vec<u8>, Error>
    where
        E: Endpoint<Response = Vec<u8>>,
    {
        let url = build_url::<E>(true, info, host, request)?;
        Ok(self
            .0
            .get(url.to_string())
            .send()
            .map_err(|e| request_error(&url, e))?
            .error_for_status()
            .map_err(|e| request_error(&url, e))?
            .bytes()
            .map_err(|e| request_error(&url, e))?
            .to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonlight_common::{
        crypto::rustcrypto::RustCryptoBackend, http::pair::PairingCryptoBackend,
    };
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{mpsc, Arc},
        thread,
    };

    #[test]
    fn request_error_shows_cause_and_endpoint_without_sensitive_query() {
        let url = url::Url::parse("https://127.0.0.1:47984/serverinfo?uniqueid=private-client&clientcert=secret-certificate").unwrap();
        let underlying = reqwest::blocking::Client::new()
            .get(url.clone())
            .header("x-test", "invalid\nvalue")
            .build()
            .unwrap_err()
            .with_url(url.clone());
        let expected_cause = underlying.source().unwrap().to_string();
        let diagnostic = request_error(&url, underlying).to_string();
        assert!(diagnostic.contains("HTTPS"));
        assert!(diagnostic.contains("https://127.0.0.1:47984/serverinfo"));
        assert!(diagnostic.contains(&expected_cause));
        for secret in [
            "?",
            "uniqueid",
            "private-client",
            "clientcert",
            "secret-certificate",
        ] {
            assert!(
                !diagnostic.contains(secret),
                "diagnostic exposed {secret}: {diagnostic}"
            );
        }
    }

    #[test]
    fn pairing_request_reaches_server_and_waits_for_approval() {
        use moonlight_common::http::pair::{phase1::PairPhase1Request, PairEndpoint, PairRequest};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::channel();
        let (approve_tx, approve_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = [0; 8192];
            let size = socket.read(&mut bytes).unwrap();
            request_tx
                .send(String::from_utf8_lossy(&bytes[..size]).into_owned())
                .unwrap();
            approve_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let body = "<root status_code=\"200\"><paired>0</paired></root>";
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let (result_tx, result_rx) = mpsc::channel();
        let client = thread::spawn(move || {
            let request = PairRequest::Phase1(PairPhase1Request {
                device_name: "KVM Client".into(),
                salt: [42; 16],
                client_certificate: Pem::new("CERTIFICATE", vec![1, 2, 3]),
            });
            let result = PinnedClient::with_defaults_long_timeout()
                .unwrap()
                .send_http::<PairEndpoint>(
                    ClientInfo {
                        unique_id: "test-client".into(),
                        uuid: uuid::Uuid::nil(),
                    },
                    &address.to_string(),
                    &request,
                );
            result_tx.send(result).unwrap();
        });
        let request = request_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let target = request
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap();
        let url = url::Url::parse(&format!("http://localhost{target}")).unwrap();
        let query: Vec<_> = url.query_pairs().collect();
        assert_eq!(url.path(), "/pair");
        assert!(query
            .iter()
            .any(|(k, v)| k == "phrase" && v == "getservercert"));
        assert!(query
            .iter()
            .any(|(k, v)| k == "devicename" && v == "KVM Client"));
        assert!(query
            .iter()
            .any(|(k, v)| k == "clientcert" && !v.is_empty()));
        assert!(result_rx.try_recv().is_err());
        approve_tx.send(()).unwrap();
        assert!(result_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok());
        client.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn pairing_url_encodes_device_name_without_injecting_parameters() {
        use moonlight_common::http::pair::{phase5::PairPhase5Request, PairEndpoint, PairRequest};

        for name in ["KVM Client", "Equipo & salón=1+#"] {
            let url = build_url::<PairEndpoint>(
                false,
                ClientInfo {
                    unique_id: "test-client".into(),
                    uuid: uuid::Uuid::nil(),
                },
                "127.0.0.1:47989",
                &PairRequest::Phase5(PairPhase5Request {
                    device_name: name.into(),
                }),
            )
            .unwrap();
            let pairs: Vec<_> = url.query_pairs().collect();
            assert_eq!(url.path(), "/pair");
            assert_eq!(url.fragment(), None);
            assert_eq!(
                pairs.iter().filter(|(key, _)| key == "devicename").count(),
                1
            );
            assert_eq!(
                pairs.iter().find(|(key, _)| key == "devicename").unwrap().1,
                name
            );
            assert_eq!(
                pairs.iter().find(|(key, _)| key == "phrase").unwrap().1,
                "pairchallenge"
            );
        }
    }

    #[test]
    fn paired_certificate_is_accepted_but_another_server_is_rejected() {
        let crypto = RustCryptoBackend;
        let (certificate, key) = crypto.generate_client_identity().unwrap();
        let (other_certificate, _) = crypto.generate_client_identity().unwrap();
        let certificate = certificate.to_pem();
        let key = key.to_pem();
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(
                certificate.contents().to_vec(),
            )],
            rustls::pki_types::PrivateKeyDer::try_from(key.contents().to_vec()).unwrap(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let connection =
                    rustls::ServerConnection::new(Arc::new(server_config.clone())).unwrap();
                let mut tls = rustls::StreamOwned::new(connection, socket);
                let mut request = [0; 2048];
                if tls.read(&mut request).is_ok() {
                    let _ = tls.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    );
                }
            }
        });
        let url = format!("https://{address}/serverinfo");
        let paired = PinnedClient::with_certificates(&key, &certificate, &certificate).unwrap();
        assert_eq!(paired.0.get(&url).send().unwrap().text().unwrap(), "ok");
        let changed =
            PinnedClient::with_certificates(&key, &certificate, &other_certificate.to_pem())
                .unwrap();
        assert!(changed.0.get(url).send().is_err());
        server.join().unwrap();
    }
}
