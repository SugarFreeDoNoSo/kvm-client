use std::{
    fmt::{self, Write as _},
    str::FromStr,
};

use roxmltree::Document;

use crate::{
    AppId,
    error::{Error, parse_error, parse_i32, parse_number_as_bool, parse_u32},
    http::{
        Endpoint, QueryBuilder, QueryBuilderError, QueryMap, QueryParam, Request, TextResponse,
        helper::{
            fmt_write_to_buffer, i32_to_str, parse_xml_child_text, parse_xml_root_node, u32_to_str,
        },
    },
    stream::{AesIv, AesKey, audio::AudioConfig},
};

/// Launches a new session.
///
/// When there's already an active game this will fail to start a new session.
/// Then you should use [super::resume::ResumeEndpoint].
pub struct LaunchEndpoint;

impl Endpoint for LaunchEndpoint {
    type Request = ClientStreamRequest;
    type Response = LaunchResponse;

    fn path() -> &'static str {
        "/launch"
    }

    fn https_required() -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClientStreamRequest {
    pub app_id: AppId,
    pub mode_width: u32,
    pub mode_height: u32,
    pub mode_fps: u32,
    pub sops: bool,
    pub hdr: bool,
    pub surround_audio_info: AudioConfig,
    pub local_audio_play_mode: bool,
    pub gamepads_attached_mask: i32,
    pub gamepads_persist_after_disconnect: bool,
    pub ri_key: AesKey,
    pub ri_key_id: AesIv,
    /// The core version:
    /// - empty / 0 = Video encryption and control stream encryption v2
    /// - 1 = RTSP encryption
    ///
    /// You can set this empty and use [Self::additional_query_parameters] if you're using moonlight-common-c.
    ///
    /// References:
    /// - <https://github.com/moonlight-stream/moonlight-common-c/blob/62687809b1f7410c3db4be2527503a54ae408d70/src/Connection.c#L550-L554>
    pub core_version: Option<u32>,
    /// Useful for serializing using moonlight-common-c: <https://github.com/moonlight-stream/moonlight-common-c/blob/62687809b1f7410c3db4be2527503a54ae408d70/src/Limelight.h#L38-L42>
    ///
    /// When deserializing this will always be empty
    pub additional_query_parameters: String,
}

impl Request for ClientStreamRequest {
    fn append_query_params(
        &self,
        query_builder: &mut impl QueryBuilder,
    ) -> Result<(), QueryBuilderError> {
        let launch_params = form_urlencoded::parse(self.additional_query_parameters.as_bytes());
        for (name, value) in launch_params {
            query_builder.append(QueryParam {
                key: &name,
                value: &value,
            })?;
        }

        let mut appid_buffer = [0u8; _];
        let appid = u32_to_str(self.app_id.0, &mut appid_buffer);
        query_builder.append(QueryParam {
            key: "appid",
            value: appid,
        })?;

        let mut mode_buffer = [0u8; (11 * 3) + 2];
        let mode = fmt_write_to_buffer(&mut mode_buffer, |writer| {
            write!(
                writer,
                "{}x{}x{}",
                self.mode_width, self.mode_height, self.mode_fps
            )
            .expect("write mode")
        });
        query_builder.append(QueryParam {
            key: "mode",
            value: mode,
        })?;

        query_builder.append(QueryParam {
            key: "additionalStates",
            value: "1",
        })?;
        query_builder.append(QueryParam {
            key: "sops",
            value: if self.sops { "1" } else { "0" },
        })?;

        if self.hdr {
            query_builder.append(QueryParam {
                key: "hdrMode",
                value: "1",
            })?;
            query_builder.append(QueryParam {
                key: "clientHdrCapVersion",
                value: "0",
            })?;
            query_builder.append(QueryParam {
                key: "clientHdrCapSupportedFlagsInUint32",
                value: "0",
            })?;
            query_builder.append(QueryParam {
                key: "clientHdrCapMetaDataId",
                value: "NV_STATIC_METADATA_TYPE_1",
            })?;
            query_builder.append(QueryParam {
                key: "clientHdrCapDisplayData",
                value: "0x0x0x0x0x0x0x0x0x0x0",
            })?;
        }

        let mut ri_key_str_bytes = [0u8; 16 * 2];
        hex::encode_to_slice(&*self.ri_key, &mut ri_key_str_bytes).expect("encode ri key");
        query_builder.append(QueryParam {
            key: "rikey",
            value: str::from_utf8(&ri_key_str_bytes).expect("valid ri key str"),
        })?;

        let mut ri_key_id_str_bytes = [0; 11];
        let ri_key_id_str = u32_to_str(*self.ri_key_id, &mut ri_key_id_str_bytes);
        query_builder.append(QueryParam {
            key: "rikeyid",
            value: ri_key_id_str,
        })?;

        query_builder.append(QueryParam {
            key: "localAudioPlayMode",
            value: if self.local_audio_play_mode { "1" } else { "0" },
        })?;

        let mut surround_audio_info = [0u8; 11];
        let surround_audio_info_value = u32_to_str(
            self.surround_audio_info.to_surround_audio_info(),
            &mut surround_audio_info,
        );
        query_builder.append(QueryParam {
            key: "surroundAudioInfo",
            value: surround_audio_info_value,
        })?;

        let mut gamepad_attached_mask_buffer = [0u8; 11];
        let gamepad_attached_mask_value = i32_to_str(
            self.gamepads_attached_mask,
            &mut gamepad_attached_mask_buffer,
        );
        query_builder.append(QueryParam {
            key: "remoteControllersBitmap",
            value: gamepad_attached_mask_value,
        })?;
        query_builder.append(QueryParam {
            key: "gcmap",
            value: gamepad_attached_mask_value,
        })?;

        query_builder.append(QueryParam {
            key: "gcpersist",
            value: if self.gamepads_persist_after_disconnect {
                "1"
            } else {
                "0"
            },
        })?;

        if let Some(core_version) = self.core_version {
            let mut core_version_str_bytes = [0u8; 11];
            let core_version_str = u32_to_str(core_version, &mut core_version_str_bytes);
            query_builder.append(QueryParam {
                key: "corever",
                value: core_version_str,
            })?;
        }

        Ok(())
    }

    fn from_query_params<Q>(query_map: &Q) -> Result<Self, Error>
    where
        Q: QueryMap,
    {
        const ERROR_CONTEXT: &str = "http query: launch";

        let app_id = parse_u32(ERROR_CONTEXT, "appid", &query_map.get("appid")?).map(AppId)?;

        let mode = query_map.get("mode")?;
        let mut mode_split = mode.split("x");
        let mode_width: u32 = parse_u32(
            ERROR_CONTEXT,
            "mode.width",
            mode_split.next().ok_or_else(|| Error::InvalidValue {
                context: ERROR_CONTEXT,
                expected: "WIDTHxHEIGHTxFPS",
                got: mode.to_string(),
            })?,
        )?;
        let mode_height: u32 = parse_u32(
            ERROR_CONTEXT,
            "mode.height",
            mode_split.next().ok_or_else(|| Error::InvalidValue {
                context: ERROR_CONTEXT,
                expected: "WIDTHxHEIGHTxFPS",
                got: mode.to_string(),
            })?,
        )?;
        let mode_fps: u32 = parse_u32(
            ERROR_CONTEXT,
            "mode.fps",
            mode_split.next().ok_or_else(|| Error::InvalidValue {
                context: ERROR_CONTEXT,
                expected: "WIDTHxHEIGHTxFPS",
                got: mode.to_string(),
            })?,
        )?;

        let sops = parse_number_as_bool(
            ERROR_CONTEXT,
            "sops",
            &query_map.get("sops").unwrap_or("0".into()),
        )?;

        let hdr = parse_number_as_bool(
            ERROR_CONTEXT,
            "hdrMode",
            &query_map.get("hdrMode").unwrap_or("0".into()),
        )?;

        let surround_audio_info_raw = query_map
            .get("surroundAudioInfo")
            .ok()
            .map(|x| parse_u32(ERROR_CONTEXT, "surroundAudioInfo", &x))
            .transpose()?
            .unwrap_or(AudioConfig::STEREO.to_surround_audio_info());
        let surround_audio_info = AudioConfig::from_surround_audio_info(surround_audio_info_raw);

        let local_audio_play_mode =
            query_map.get("localAudioPlayMode").unwrap_or("1".into()) != "0";

        // TODO: what to trust?
        let _gamepads_attached_mask: u32 = parse_u32(
            ERROR_CONTEXT,
            "remoteControllersBitmap",
            query_map
                .get("remoteControllersBitmap")
                .as_deref()
                .unwrap_or("0"),
        )?;
        let gamepads_attached_mask = parse_i32(
            ERROR_CONTEXT,
            "gcmap",
            query_map.get("gcmap").as_deref().unwrap_or("0"),
        )?;

        let gamepads_persist_after_disconnect =
            query_map.get("gcpersist").unwrap_or("0".into()) != "0";

        let mut ri_key = [0u8; _];
        let ri_key_hex = query_map.get("rikey")?;
        hex::decode_to_slice(ri_key_hex.as_bytes(), &mut ri_key).map_err(|_| {
            parse_error(
                ERROR_CONTEXT,
                "rikey",
                "valid hex bytes that resolve to 16 bytes",
                ri_key_hex.to_string(),
            )
        })?;

        let ri_key_id: u32 = parse_u32(ERROR_CONTEXT, "rikeyid", &query_map.get("rikeyid")?)?;

        let core_version: Option<u32> = query_map
            .get("corever")
            .ok()
            .map(|x| parse_u32(ERROR_CONTEXT, "corever", &x))
            .transpose()?;

        Ok(Self {
            app_id,
            mode_width,
            mode_height,
            mode_fps,
            sops,
            hdr,
            surround_audio_info,
            local_audio_play_mode,
            gamepads_attached_mask,
            gamepads_persist_after_disconnect,
            ri_key: AesKey(ri_key),
            ri_key_id: AesIv(ri_key_id),
            core_version,
            additional_query_parameters: String::new(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchResponse {
    // TODO: what exactly is game_session used for?
    pub game_session: u32,
    pub rtsp_session_url: Option<String>,
}

impl TextResponse for LaunchResponse {
    fn serialize_into(&self, body_writer: &mut impl fmt::Write) -> fmt::Result {
        // XML header + root
        body_writer.write_str(r#"<?xml version="1.0" encoding="utf-8"?>"#)?;
        body_writer.write_str(r#"<root status_code="200">"#)?;

        // <gamesession>
        write!(
            body_writer,
            "<gamesession>{}</gamesession>",
            self.game_session
        )?;

        // <sessionUrl0>
        if let Some(rtsp_session_url) = &self.rtsp_session_url {
            write!(
                body_writer,
                "<sessionUrl0>{}</sessionUrl0>",
                rtsp_session_url
            )?;
        }

        // close root
        body_writer.write_str("</root>")?;

        Ok(())
    }
}

impl FromStr for LaunchResponse {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        const ERROR_CONTEXT: &str = "http xml: launch";

        let doc = Document::parse(s)?;
        let root = parse_xml_root_node(&doc)?;

        let rtsp_session_url = match parse_xml_child_text(root, "sessionUrl0") {
            Ok(value) => Some(value.to_string()),
            Err(Error::MissingAttribute { .. }) => None,
            Err(err) => {
                return Err(err);
            }
        };

        Ok(LaunchResponse {
            game_session: parse_u32(
                ERROR_CONTEXT,
                "gamesession",
                parse_xml_child_text(root, "gamesession")?,
            )?,
            rtsp_session_url,
        })
    }
}
