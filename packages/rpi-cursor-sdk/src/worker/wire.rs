use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(super) const MAX_FRAME: usize = 64 * 1024 * 1024;
pub(super) const CAPACITY: usize = 64;
pub(super) const PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_CALLS: usize = 16;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum Body {
    Request {
        method: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
    Reply {
        result: Value,
        #[serde(rename = "lastEvent")]
        last_event: u64,
    },
    Error {
        error: String,
        #[serde(rename = "lastEvent")]
        last_event: u64,
    },
    Event {
        sequence: u64,
        event: Value,
    },
    Credit {
        sequence: u64,
    },
    Cancel,
}

pub(super) fn message(generation: &str, id: &str, body: Body) -> Value {
    let mut value = serde_json::to_value(body).expect("worker body serializes");
    value["version"] = 1.into();
    value["generation"] = generation.into();
    value["id"] = id.into();
    value
}

pub(super) fn decode(mut value: Value, generation: &str) -> Result<(String, Body), String> {
    let object = value.as_object_mut().ok_or("invalid worker frame")?;
    if object.remove("version") != Some(1.into())
        || object.remove("generation") != Some(generation.into())
    {
        return Err("invalid worker generation or version".into());
    }
    let id = object
        .remove("id")
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or("invalid worker request identity")?;
    if !(id.starts_with('h') || id.starts_with('w')) || id_number(&id).is_none() {
        return Err("invalid worker request identity".into());
    }
    if object
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| {
            (kind == "request" && !object.contains_key("input"))
                || (kind == "reply" && !object.contains_key("result"))
                || (kind == "event" && !object.contains_key("event"))
        })
    {
        return Err("missing worker frame payload".into());
    }
    let fields: &[&str] = match object.get("kind").and_then(Value::as_str) {
        Some("request") => &["kind", "method", "input", "parent"],
        Some("reply") => &["kind", "result", "lastEvent"],
        Some("error") => &["kind", "error", "lastEvent"],
        Some("event") => &["kind", "sequence", "event"],
        Some("credit") => &["kind", "sequence"],
        Some("cancel") => &["kind"],
        _ => return Err("invalid worker frame kind".into()),
    };
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err("unknown worker frame field".into());
    }
    let body = serde_json::from_value(value).map_err(|_| "invalid worker frame body")?;
    Ok((id, body))
}

pub(super) fn id_number(id: &str) -> Option<u64> {
    let digits = id.get(1..)?;
    if digits.is_empty()
        || digits.len() > 16
        || digits.starts_with('0')
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    digits.parse().ok()
}

pub(super) fn encode(value: &Value) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(value).map_err(|_| "worker frame is not JSON")?;
    if json.is_empty() || json.len() > MAX_FRAME {
        return Err("worker frame exceeds size limit".into());
    }
    let mut bytes = Vec::with_capacity(json.len() + 4);
    bytes.extend_from_slice(&(json.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&json);
    Ok(bytes)
}

pub(super) async fn read(
    stream: &mut (impl AsyncRead + Unpin),
    limit: usize,
) -> Result<(Value, usize), String> {
    let size = stream
        .read_u32()
        .await
        .map_err(|_| "worker connection closed or truncated")? as usize;
    if size == 0 || size > limit {
        return Err("invalid worker frame length".into());
    }
    let mut bytes = vec![0; size];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| "worker frame truncated")?;
    let value = serde_json::from_slice(&bytes).map_err(|_| "invalid worker frame JSON")?;
    Ok((value, size))
}

pub(super) async fn write(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &Value,
) -> Result<(), String> {
    stream
        .write_all(&encode(value)?)
        .await
        .map_err(|_| "worker write failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn fragmented_frames_and_invalid_boundaries() {
        let value = message(
            "generation",
            "h1",
            Body::Request {
                method: "echo".into(),
                input: json!("🦀"),
                parent: None,
            },
        );
        let frame = encode(&value).unwrap();
        let (mut writer, mut reader) = tokio::io::duplex(1);
        tokio::spawn(async move {
            for byte in frame {
                writer.write_all(&[byte]).await.unwrap();
            }
        });
        let (decoded, _) = read(&mut reader, MAX_FRAME).await.unwrap();
        assert_eq!(decoded, value);
        assert!(decode(decoded.clone(), "other").is_err());
        assert!(decode(decoded, "generation").is_ok());
        for bytes in [
            vec![0, 0],
            vec![0, 0, 0, 0],
            (MAX_FRAME as u32 + 1).to_be_bytes().to_vec(),
            vec![0, 0, 0, 3, b'"', 0xff, b'"'],
        ] {
            assert!(read(&mut bytes.as_slice(), MAX_FRAME).await.is_err());
        }
    }

    #[test]
    fn malformed_bodies_are_not_treated_as_null_or_future_capabilities() {
        for body in [
            json!({"kind":"request","method":"echo"}),
            json!({"kind":"reply","lastEvent":0}),
            json!({"kind":"event","sequence":1}),
            json!({"kind":"cancel","token":"not a capability"}),
            json!({"kind":"reply","result":null,"lastEvent":-1}),
        ] {
            let mut value = body;
            value["version"] = 1.into();
            value["generation"] = "g".into();
            value["id"] = "h1".into();
            assert!(decode(value, "g").is_err());
        }
        for id in ["h0", "h-1", "h01", "h10000000000000000", "h", "h1x"] {
            assert!(id_number(id).is_none());
        }
    }
}
