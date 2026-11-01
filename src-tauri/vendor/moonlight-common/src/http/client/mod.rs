use std::time::Duration;

pub mod async_client;
pub mod blocking_client;

#[cfg(feature = "ureq")]
pub mod ureq;

#[cfg(feature = "tokio-hyper")]
pub mod tokio_hyper;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_LONG_TIMEOUT: Duration = Duration::from_secs(90);

#[cfg(any(feature = "ureq", feature = "tokio-hyper", feature = "http-client"))]
pub mod hyperlike {
    use http::Uri;

    use crate::{
        error::Error,
        http::{ClientInfo, Endpoint, QueryBuilder, QueryBuilderError, QueryParam, Request},
    };

    struct StringQueryBuilder<'a> {
        is_first: bool,
        string: &'a mut String,
    }

    impl QueryBuilder for StringQueryBuilder<'_> {
        fn append(&mut self, param: QueryParam) -> Result<(), QueryBuilderError> {
            if !self.is_first {
                self.string.push('&');
            }
            self.is_first = false;

            self.string.push_str(param.key);
            self.string.push('=');
            self.string.push_str(param.value);

            Ok(())
        }
    }

    pub fn build_url<E>(
        use_https: bool,
        client_info: ClientInfo,
        hostport: &str,
        request: &E::Request,
    ) -> Result<Uri, Error>
    where
        E: Endpoint,
    {
        let mut path_and_query = format!("{}?", E::path());

        let mut query_builder = StringQueryBuilder {
            is_first: true,
            string: &mut path_and_query,
        };

        // This cannot fail
        #[allow(clippy::unwrap_used)]
        client_info.append_query_params(&mut query_builder).unwrap();
        #[allow(clippy::unwrap_used)]
        request.append_query_params(&mut query_builder).unwrap();

        let uri = Uri::builder()
            .scheme(if use_https { "https" } else { "http" })
            .authority(hostport)
            .path_and_query(path_and_query)
            .build()
            .map_err(|err| Error::Other(err.into()))?;

        Ok(uri)
    }
}
