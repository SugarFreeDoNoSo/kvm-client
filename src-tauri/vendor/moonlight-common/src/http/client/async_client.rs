use pem::Pem;

use crate::{
    error::Error,
    http::{ClientInfo, Endpoint, TextResponse},
};

///
/// An async request client that can make requests to an [Endpoint].
///
pub trait RequestClient: Sized + Clone {
    fn with_defaults() -> Result<Self, Error>;
    fn with_defaults_long_timeout() -> Result<Self, Error>;

    fn with_certificates(
        client_private_key: &Pem,
        client_certificate: &Pem,
        server_certificate: &Pem,
    ) -> Result<Self, Error>;

    fn send_http<E>(
        &self,
        client_info: ClientInfo,
        hostport: &str,
        request: &E::Request,
    ) -> impl Future<Output = Result<E::Response, Error>>
    where
        E: Endpoint,
        E::Request: Sync,
        E::Response: TextResponse;

    fn send_https<E>(
        &self,
        client_info: ClientInfo,
        hostport: &str,
        request: &E::Request,
    ) -> impl Future<Output = Result<E::Response, Error>>
    where
        E: Endpoint,
        E::Request: Sync,
        E::Response: TextResponse;

    fn send_https_with_bytes<E>(
        &self,
        client_info: ClientInfo,
        hostport: &str,
        request: &E::Request,
    ) -> impl Future<Output = Result<E::Response, Error>>
    where
        E: Endpoint<Response = Vec<u8>>,
        E::Request: Sync;
}
