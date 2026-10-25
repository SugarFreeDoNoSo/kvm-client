//! Luckfox WebSocket signaling wraps the JSON SDP object in base64.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

#[derive(Debug, PartialEq)]
pub enum Signal {
    Metadata,
    Answer(String),
    Ice { candidate: String, index: u32 },
    Ping,
    Ignore,
}

pub fn offer(sdp: &str) -> Value {
    let description = json!({"type":"offer", "sdp":sdp});
    json!({"type":"offer", "data":{"sd":STANDARD.encode(description.to_string())}})
}

pub fn ice(candidate: &str, index: u32) -> Value {
    json!({"type":"new-ice-candidate", "data":{"candidate":candidate,"sdpMLineIndex":index}})
}

pub fn parse(text: &str) -> Result<Signal, String> {
    if text == "ping" {
        return Ok(Signal::Ping);
    }
    if text == "pong" {
        return Ok(Signal::Ignore);
    }
    let message: Value =
        serde_json::from_str(text).map_err(|e| format!("Señal WebRTC inválida: {e}"))?;
    match message["type"].as_str() {
        Some("device-metadata") => Ok(Signal::Metadata),
        Some("answer") => {
            let data = message["data"].as_str().ok_or("SDP de respuesta ausente")?;
            let decoded = STANDARD.decode(data).map_err(|e| e.to_string())?;
            let description: Value = serde_json::from_slice(&decoded).map_err(|e| e.to_string())?;
            if description["type"] != "answer" {
                return Err("Tipo SDP de respuesta incorrecto".into());
            }
            Ok(Signal::Answer(
                description["sdp"].as_str().ok_or("SDP ausente")?.to_owned(),
            ))
        }
        Some("new-ice-candidate") => {
            let candidate = message["data"]["candidate"]
                .as_str()
                .ok_or("Candidato ICE ausente")?;
            if candidate.is_empty() {
                return Ok(Signal::Ignore);
            }
            let index = message["data"]["sdpMLineIndex"].as_u64().unwrap_or(0);
            if index > u32::MAX as u64 {
                return Err("Índice ICE inválido".into());
            }
            Ok(Signal::Ice {
                candidate: candidate.to_owned(),
                index: index as u32,
            })
        }
        _ => Ok(Signal::Ignore),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offer_matches_device_wire_format() {
        let message = offer("v=0\r\n");
        assert_eq!(message["type"], "offer");
        let decoded = STANDARD
            .decode(message["data"]["sd"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&decoded).unwrap(),
            json!({"type":"offer","sdp":"v=0\r\n"})
        );
    }
    #[test]
    fn answer_and_ice_parse_with_ping_keepalive() {
        let answer =
            json!({"type":"answer","data":STANDARD.encode(r#"{"type":"answer","sdp":"v=0"}"#)});
        assert_eq!(
            parse(&answer.to_string()).unwrap(),
            Signal::Answer("v=0".into())
        );
        assert_eq!(
            parse(&ice("candidate:1", 2).to_string()).unwrap(),
            Signal::Ice {
                candidate: "candidate:1".into(),
                index: 2
            }
        );
        assert_eq!(parse("ping").unwrap(), Signal::Ping);
        assert!(parse(r#"{"type":"answer","data":"%%%"}"#).is_err());
    }
}
