use thiserror::Error;

use crate::{
    error::Error,
    http::{
        ClientIdentifier, ClientSecret,
        pair::{HashAlgorithm, PairingCryptoBackend},
    },
};

#[derive(Debug, Error)]
#[error("the cryptography operations have been disabled")]
pub struct CryptoBackendDisabledError;

impl From<CryptoBackendDisabledError> for Error {
    fn from(value: CryptoBackendDisabledError) -> Self {
        Self::Other(value.into())
    }
}

#[derive(Debug, Clone)]
pub struct DisabledCryptoBackend;

impl PairingCryptoBackend for DisabledCryptoBackend {
    fn generate_client_identity(&self) -> Result<(ClientIdentifier, ClientSecret), Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn hash(
        &self,
        _algorithm: HashAlgorithm,
        _data: &[u8],
        _output: &mut [u8],
    ) -> Result<(), Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn random_bytes(&self, _data: &mut [u8]) -> Result<(), Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn encrypt_aes(&self, _key: &[u8], _plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn decrypt_aes(&self, _key: &[u8], _ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn client_signature(
        &self,
        _client_certificate: &crate::http::ClientIdentifier,
    ) -> Result<Vec<u8>, Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn server_signature(
        &self,
        _server_certificate: &crate::http::ServerIdentifier,
    ) -> Result<Vec<u8>, Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn verify_signature(
        &self,
        _server_secret: &[u8],
        _server_signature: &[u8],
        _server_certificate: &crate::http::ServerIdentifier,
    ) -> Result<bool, Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn sign_data(
        &self,
        _private_key: &crate::http::ClientSecret,
        _data: &[u8],
    ) -> Result<Vec<u8>, Error> {
        Err(CryptoBackendDisabledError.into())
    }
}

#[cfg(feature = "stream-proto")]
use crate::stream::proto::crypto::CryptoBackend;

#[cfg(feature = "stream-proto")]
impl CryptoBackend for DisabledCryptoBackend {
    fn encrypt_aes_gcm(
        &self,
        _key: &[u8],
        _iv: &[u8],
        _input: &[u8],
        _output: &mut [u8],
        _tag: &mut [u8],
    ) -> Result<(), Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn decrypt_aes_gcm(
        &self,
        _key: &[u8],
        _iv: &[u8],
        _input: &[u8],
        _tag: &[u8],
        _output: &mut [u8],
    ) -> Result<(), Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn encrypt_aes_cbc(
        &self,
        _key: &[u8],
        _iv: &[u8],
        _input: &[u8],
        _output: &mut [u8],
    ) -> Result<usize, Error> {
        Err(CryptoBackendDisabledError.into())
    }

    fn decrypt_aes_cbc(
        &self,
        _key: &[u8],
        _iv: &[u8],
        _input: &[u8],
        _output: &mut [u8],
    ) -> Result<usize, Error> {
        Err(CryptoBackendDisabledError.into())
    }
}
