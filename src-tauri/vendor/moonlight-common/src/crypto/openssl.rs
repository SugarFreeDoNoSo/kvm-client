//!
//! This module contains openssl implementations for the CryptoBackends this library has.
//!

use std::str::FromStr;

use openssl::{
    asn1::Asn1Time,
    cipher::Cipher,
    cipher_ctx::CipherCtx,
    error::ErrorStack,
    hash::MessageDigest,
    md::Md,
    md_ctx::MdCtx,
    pkey::{PKey, Private},
    rand::rand_bytes,
    rsa::Rsa,
    sha::{sha1, sha256},
    x509::{X509, X509Builder, X509NameBuilder},
};
use pem::Pem;
use tracing::{Level, instrument, trace};

use crate::{
    error::Error,
    http::{
        ClientIdentifier, ClientSecret, ServerIdentifier,
        pair::{HashAlgorithm, PairingCryptoBackend},
    },
};

impl From<ErrorStack> for Error {
    fn from(value: ErrorStack) -> Self {
        // TODO: check for verification failed
        Self::Other(value.into())
    }
}

#[derive(Debug, Clone)]
pub struct OpenSSLCryptoBackend;

impl PairingCryptoBackend for OpenSSLCryptoBackend {
    #[cfg_attr(not(feature = "__tracing_sensitive"), instrument(level = Level::TRACE, skip_all, err))]
    #[cfg_attr(feature = "__tracing_sensitive", instrument(level = Level::TRACE, skip(self, output), ret, err))]
    fn hash(&self, algorithm: HashAlgorithm, data: &[u8], output: &mut [u8]) -> Result<(), Error> {
        match algorithm {
            HashAlgorithm::Sha1 => {
                let digest = sha1(data);
                output.copy_from_slice(&digest);
            }
            HashAlgorithm::Sha256 => {
                let digest = sha256(data);
                output.copy_from_slice(&digest);
            }
        }

        trace!(output = ?output);

        Ok(())
    }

    #[cfg_attr(not(feature = "__tracing_sensitive"), instrument(level = Level::TRACE, skip_all, err))]
    #[cfg_attr(feature = "__tracing_sensitive", instrument(level = Level::TRACE, skip(self, data), ret, err))]
    fn random_bytes(&self, data: &mut [u8]) -> Result<(), Error> {
        rand_bytes(data)?;

        trace!(data = ?data);

        Ok(())
    }

    #[cfg_attr(not(feature = "__tracing_sensitive"), instrument(level = Level::TRACE, skip_all, err))]
    #[cfg_attr(feature = "__tracing_sensitive", instrument(level = Level::TRACE, skip(self), ret, err))]
    fn generate_client_identity(&self) -> Result<(ClientIdentifier, ClientSecret), Error> {
        let rsa = Rsa::generate(2048)?;
        let key = PKey::from_rsa(rsa)?;

        let private_key_pem = String::from_utf8(key.private_key_to_pem_pkcs8()?)
            .expect("valid openssl private key pem");

        // Build X.509 Name
        let mut name = X509NameBuilder::new()?;
        name.append_entry_by_text("C", "US")?;
        name.append_entry_by_text("ST", "CA")?;
        name.append_entry_by_text("L", "San Francisco")?;
        name.append_entry_by_text("O", "Example Corp")?;
        name.append_entry_by_text("CN", "example.com")?;
        let name = name.build();

        // Build certificate
        let mut builder = X509Builder::new()?;
        builder.set_version(2)?; // X509 v3
        builder.set_subject_name(&name)?;
        builder.set_issuer_name(&name)?;
        builder.set_pubkey(&key)?;
        builder.set_not_before(Asn1Time::days_from_now(0)?.as_ref())?;
        builder.set_not_after(Asn1Time::days_from_now(365)?.as_ref())?;
        builder.sign(&key, MessageDigest::sha256())?;
        let certificate = builder.build();

        let certificate_pem =
            String::from_utf8(certificate.to_pem()?).expect("valid openssl certificate pem");

        Ok((
            // We're trusting openssl to give us correct values
            #[allow(clippy::unwrap_used)]
            ClientIdentifier::from_pem(Pem::from_str(&certificate_pem).unwrap()),
            #[allow(clippy::unwrap_used)]
            ClientSecret::from_pem(Pem::from_str(&private_key_pem).unwrap()),
        ))
    }

    #[cfg_attr(not(feature = "__tracing_sensitive"), instrument(level = Level::TRACE, skip_all, err))]
    #[cfg_attr(feature = "__tracing_sensitive", instrument(level = Level::TRACE, skip(self), ret, err))]
    fn encrypt_aes(&self, key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        let mut cipher_ctx = CipherCtx::new()?;

        cipher_ctx.encrypt_init(Some(Cipher::aes_128_ecb()), Some(key), None)?;
        cipher_ctx.set_padding(false);

        let mut output = Vec::new();
        cipher_ctx.cipher_update_vec(plaintext, &mut output)?;
        Ok(output)
    }

    #[cfg_attr(not(feature = "__tracing_sensitive"), instrument(level = Level::TRACE, skip_all, err))]
    #[cfg_attr(feature = "__tracing_sensitive", instrument(level = Level::TRACE, skip(self), ret, err))]
    fn decrypt_aes(&self, key: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
        let mut cipher_ctx = CipherCtx::new()?;

        cipher_ctx.decrypt_init(Some(Cipher::aes_128_ecb()), Some(key), None)?;
        cipher_ctx.set_padding(false);

        let mut decrypted = Vec::new();
        cipher_ctx.cipher_update_vec(ciphertext, &mut decrypted)?;

        Ok(decrypted)
    }

    #[cfg_attr(not(feature = "__tracing_sensitive"), instrument(level = Level::TRACE, skip_all, err))]
    #[cfg_attr(feature = "__tracing_sensitive", instrument(level = Level::TRACE, skip(self), ret, err))]
    fn client_signature(&self, client_certificate: &ClientIdentifier) -> Result<Vec<u8>, Error> {
        let client_certificate = X509::from_der(client_certificate.to_pem().contents())?;

        Ok(client_certificate.signature().as_slice().to_vec())
    }

    #[cfg_attr(not(feature = "__tracing_sensitive"), instrument(level = Level::TRACE, skip_all, err))]
    #[cfg_attr(feature = "__tracing_sensitive", instrument(level = Level::TRACE, skip(self), ret, err))]
    fn server_signature(&self, server_certificate: &ServerIdentifier) -> Result<Vec<u8>, Error> {
        let server_certificate = X509::from_der(server_certificate.to_pem().contents())?;

        Ok(server_certificate.signature().as_slice().to_vec())
    }

    #[cfg_attr(not(feature = "__tracing_sensitive"), instrument(level = Level::TRACE, skip_all, err))]
    #[cfg_attr(feature = "__tracing_sensitive", instrument(level = Level::TRACE, skip(self), ret, err))]
    fn verify_signature(
        &self,
        server_secret: &[u8],
        server_signature: &[u8],
        server_identifier: &ServerIdentifier,
    ) -> Result<bool, Error> {
        let server_certificate = X509::from_der(server_identifier.to_pem().contents())?;

        let public_key = server_certificate.public_key()?;

        let mut md_ctx = MdCtx::new()?;

        md_ctx.digest_verify_init(Some(Md::sha256()), &public_key)?;
        md_ctx.digest_verify_update(server_secret)?;
        let result = md_ctx.digest_verify_final(server_signature)?;

        Ok(result)
    }

    #[cfg_attr(not(feature = "__tracing_sensitive"), instrument(level = Level::TRACE, skip_all, err))]
    #[cfg_attr(feature = "__tracing_sensitive", instrument(level = Level::TRACE, skip(self), ret, err))]
    fn sign_data(&self, private_key: &ClientSecret, data: &[u8]) -> Result<Vec<u8>, Error> {
        let private_key = PKey::<Private>::private_key_from_der(private_key.to_pem().contents())?;

        let mut md_ctx = MdCtx::new()?;

        md_ctx.digest_sign_init(Some(Md::sha256()), &private_key)?;
        md_ctx.digest_sign_update(data)?;

        let mut out = Vec::new();
        md_ctx.digest_sign_final_to_vec(&mut out)?;

        Ok(out)
    }
}

#[cfg(feature = "stream-proto")]
mod proto {
    use openssl::symm::{self, Crypter, Mode};

    use crate::{
        crypto::openssl::OpenSSLCryptoBackend, error::Error, stream::proto::crypto::CryptoBackend,
    };

    impl CryptoBackend for OpenSSLCryptoBackend {
        fn encrypt_aes_gcm(
            &self,
            key: &[u8],
            iv: &[u8],
            input: &[u8],
            output: &mut [u8],
            tag: &mut [u8],
        ) -> Result<(), Error> {
            let cipher = symm::Cipher::aes_128_gcm();

            let mut crypter = Crypter::new(cipher, Mode::Encrypt, key, Some(iv))?;

            let mut count = crypter.update(input, output)?;
            count += crypter.finalize(&mut output[count..])?;

            crypter.get_tag(tag)?;

            debug_assert_eq!(count, input.len());
            Ok(())
        }

        fn decrypt_aes_gcm(
            &self,
            key: &[u8],
            iv: &[u8],
            input: &[u8],
            tag: &[u8],
            output: &mut [u8],
        ) -> Result<(), Error> {
            let cipher = symm::Cipher::aes_128_gcm();

            let mut crypter = Crypter::new(cipher, Mode::Decrypt, key, Some(iv))?;
            crypter.pad(false);

            let mut count = crypter.update(input, output)?;

            crypter.set_tag(tag)?;
            count += crypter.finalize(&mut output[count..])?;

            debug_assert_eq!(count, input.len());
            Ok(())
        }

        fn encrypt_aes_cbc(
            &self,
            key: &[u8],
            iv: &[u8],
            input: &[u8],
            output: &mut [u8],
        ) -> Result<usize, Error> {
            let cipher = symm::Cipher::aes_128_cbc();

            // encrypt
            let mut crypter = Crypter::new(cipher, Mode::Encrypt, key, Some(iv))?;
            crypter.pad(true);

            let mut count = crypter.update(input, output)?;
            count += crypter.finalize(&mut output[count..])?;

            Ok(count)
        }

        fn decrypt_aes_cbc(
            &self,
            key: &[u8],
            iv: &[u8],
            input: &[u8],
            output: &mut [u8],
        ) -> Result<usize, Error> {
            let cipher = symm::Cipher::aes_128_cbc();

            // decrypt
            let mut crypter = Crypter::new(cipher, Mode::Decrypt, key, Some(iv))?;
            crypter.pad(true);

            let mut count = crypter.update(input, output)?;
            count += crypter.finalize(&mut output[count..])?;

            Ok(count)
        }
    }

    #[cfg(test)]
    mod test {
        use crate::stream::proto::crypto::test::{
            test_aes_cbc_roundtrip, test_aes_gcm_roundtrip_nonce_12,
            test_aes_gcm_roundtrip_nonce_16,
        };

        use super::*;

        #[test]
        fn openssl_cbc() {
            let backend = OpenSSLCryptoBackend;
            test_aes_cbc_roundtrip(&backend);
        }

        #[test]
        fn openssl_gcm_nonce_12() {
            let backend = OpenSSLCryptoBackend;
            test_aes_gcm_roundtrip_nonce_12(&backend);
        }
        #[test]
        fn openssl_gcm_nonce_16() {
            let backend = OpenSSLCryptoBackend;
            test_aes_gcm_roundtrip_nonce_16(&backend);
        }
    }
}
