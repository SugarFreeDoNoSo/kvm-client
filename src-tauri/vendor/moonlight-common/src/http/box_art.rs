use crate::{
    AppId,
    error::{Error, parse_i32, parse_u32},
    http::{
        Endpoint, QueryBuilder, QueryBuilderError, QueryMap, QueryParam, Request,
        helper::u32_to_str,
    },
};

pub struct AppBoxArtEndpoint;

impl Endpoint for AppBoxArtEndpoint {
    type Request = AppBoxArtRequest;
    type Response = Vec<u8>;

    fn path() -> &'static str {
        "/appasset"
    }

    fn https_required() -> bool {
        true
    }
}

/// Requests app assets:
/// - Get the app image: asset_type=2, asset_idx=0
#[derive(Debug, Clone, PartialEq)]
pub struct AppBoxArtRequest {
    pub app_id: AppId,
    /// Default: 2
    pub asset_type: i32,
    /// Default: 0
    pub asset_idx: i32,
}

impl Request for AppBoxArtRequest {
    fn append_query_params(
        &self,
        query_builder: &mut impl QueryBuilder,
    ) -> Result<(), QueryBuilderError> {
        let mut appid_buffer = [0u8; _];
        let appid = u32_to_str(self.app_id.0, &mut appid_buffer);
        query_builder.append(QueryParam {
            key: "appid",
            value: appid,
        })?;

        query_builder.append(QueryParam {
            key: "AssetType",
            value: "2",
        })?;
        query_builder.append(QueryParam {
            key: "AssetIdx",
            value: "0",
        })?;

        Ok(())
    }

    fn from_query_params<Q>(query_map: &Q) -> Result<Self, Error>
    where
        Q: QueryMap,
    {
        const ERROR_CONTEXT: &str = "http query: box art";

        let app_id = parse_u32(ERROR_CONTEXT, "appid", &query_map.get("appid")?).map(AppId)?;

        let asset_type: i32 = parse_i32(ERROR_CONTEXT, "AssetType", &query_map.get("AssetType")?)?;
        let asset_idx: i32 = parse_i32(ERROR_CONTEXT, "AssetIdx", &query_map.get("AssetIdx")?)?;

        Ok(Self {
            app_id,
            asset_type,
            asset_idx,
        })
    }
}
