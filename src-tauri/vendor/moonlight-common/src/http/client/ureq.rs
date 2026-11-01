use std::{str::FromStr, sync::Arc};

use pem::Pem;
use tracing::{debug, instrument, trace};
use ureq::{
    Agent,
    config::Config,
    tls::{Certificate, ClientCert, PrivateKey, RootCerts, TlsConfig, TlsProvider},
};

use crate::{
    error::Error,
    http::{
        ClientInfo, Endpoint, TextResponse,
        client::{
            DEFAULT_LONG_TIMEOUT, DEFAULT_TIMEOUT, blocking_client::RequestClient,
            hyperlike::build_url,
        },
    },
};

pub type UreqClient = Config;

impl From<ureq::Error> for Error {
    fn from(value: ureq::Error) -> Self {
        use ureq::Error;

        match value {
            Error::Timeout(_) => Self::ConnectionTimeout,
            Error::TlsRequired => Self::Unauthenticated,
            Error::HostNotFound => Self::ConnectionFailed,
            Error::ConnectionFailed => Self::ConnectionFailed,
            Error::StatusCode(code) => Self::StatusCode {
                code: code as i32,
                reason: Default::default(),
            },
            Error::Io(io) => Self::Io(io),
            Error::Other(other) => Self::Other(other),
            other => Self::Other(other.into()),
        }
    }
}

impl RequestClient for UreqClient {
    fn with_defaults() -> Result<Self, Error> {
        let config = Agent::config_builder()
            .timeout_global(Some(DEFAULT_TIMEOUT))
            .build();

        Ok(config)
    }
    fn with_defaults_long_timeout() -> Result<Self, Error> {
        let config = Agent::config_builder()
            .timeout_global(Some(DEFAULT_LONG_TIMEOUT))
            .build();

        Ok(config)
    }

    #[cfg_attr(
        not(feature = "__tracing_sensitive"),
        instrument(target = "moonlight::client::ureq", skip_all, err)
    )]
    #[cfg_attr(
        feature = "__tracing_sensitive",
        instrument(target = "moonlight::client::ureq", err)
    )]
    fn with_certificates(
        client_private_key: &Pem,
        client_certificate: &Pem,
        server_certificate: &Pem,
    ) -> Result<Self, Error> {
        let client_certificate = Certificate::from_der(client_certificate.contents()).to_owned();
        let client_private_key = PrivateKey::from_pem(client_private_key.to_string().as_bytes())?;

        let server_certificate = Certificate::from_der(server_certificate.contents()).to_owned();

        let config = Agent::config_builder()
            .timeout_global(Some(DEFAULT_TIMEOUT))
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::Rustls)
                    .client_cert(Some(ClientCert::new_with_certs(
                        &[client_certificate],
                        client_private_key,
                    )))
                    .root_certs(RootCerts::Specific(Arc::new(vec![server_certificate])))
                    // TODO: THIS MUST BE CHANGED
                    .disable_verification(true)
                    .build(),
            )
            .build();

        Ok(config)
    }

    #[instrument(target = "moonlight::client::ureq", skip(self, request), fields(path = E::path()), err)]
    fn send_http<E>(
        &self,
        client_info: ClientInfo,
        hostport: &str,
        request: &E::Request,
    ) -> Result<E::Response, Error>
    where
        E: Endpoint,
        E::Response: TextResponse,
    {
        let url = build_url::<E>(false, client_info, hostport, request)?;

        debug!(url = %url,"sending request");

        let request_builder = Agent::new_with_config(self.clone()).get(url);
        let response = request_builder.call()?;
        let response_text = response.into_body().read_to_string()?;

        debug!(response = ?response_text, "received response");

        let response = E::Response::from_str(&response_text)?;

        trace!(parsed_response = ?response, "parsed response");

        Ok(response)
    }

    #[instrument(target = "moonlight::client::ureq", skip(self, request), fields(path = E::path()), err)]
    fn send_https<E>(
        &self,
        client_info: ClientInfo,
        hostport: &str,
        request: &E::Request,
    ) -> Result<E::Response, Error>
    where
        E: Endpoint,
        E::Response: TextResponse,
    {
        let url = build_url::<E>(true, client_info, hostport, request)?;

        debug!(url = %url,"sending request");

        let request_builder = Agent::new_with_config(self.clone()).get(url);
        let response = request_builder.call()?;
        let response_text = response.into_body().read_to_string()?;

        debug!(response = ?response_text, "received response");

        let response = E::Response::from_str(&response_text)?;

        trace!(parsed_response = ?response, "parsed response");

        Ok(response)
    }

    #[instrument(target = "moonlight::client::ureq", skip(self, request), fields(path = E::path()), err)]
    fn send_https_with_bytes<E>(
        &self,
        client_info: ClientInfo,
        hostport: &str,
        request: &E::Request,
    ) -> Result<E::Response, Error>
    where
        E: Endpoint<Response = Vec<u8>>,
    {
        let url = build_url::<E>(true, client_info, hostport, request)?;

        debug!(url = %url,"sending request");

        let request_builder = Agent::new_with_config(self.clone()).get(url);
        let response = request_builder.call()?;
        let response_bytes = response.into_body().read_to_vec()?;

        Ok(response_bytes)
    }
}
