//! Ordered HTTP/1.1 wire framing required by the AGY CLI endpoint.
use crate::json::trim;
use indexmap::IndexMap;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;
use url::Url;

pub const RESPONSE_HEADER_TIMEOUT_MS: u64 = 180_000;
pub const IDLE_TIMEOUT_MS: u64 = 180_000;

pub fn user_agent(version: &str, platform: &str, arch: &str, auth_method: &str) -> String {
    let platform = match platform {
        "win32" => "windows",
        "" => "unknown",
        other => other,
    };
    let arch = match arch {
        "x64" => "amd64",
        "ia32" => "386",
        "" => "unknown",
        other => other,
    };
    let change = if version == "1.3.5" {
        "; cl=998863838"
    } else {
        ""
    };
    format!("antigravity/cli/{version} (aidev_client; os_type={platform}; arch={arch}{change}; auth_method={auth_method})")
}
pub fn default_user_agent() -> String {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "ia32",
        "aarch64" => "arm64",
        other => other,
    };
    user_agent("1.3.5", platform, arch, "consumer")
}
fn header_values(headers: &[(String, String)]) -> Result<IndexMap<String, String>, String> {
    for value in headers.iter().flat_map(|(name, value)| [name, value]) {
        if let Some((index, value)) = value
            .encode_utf16()
            .enumerate()
            .find(|(_, value)| *value > 255)
        {
            return Err(format!("Cannot convert argument to a ByteString because the character at index {index} has a value of {value} which is greater than 255."));
        }
    }
    let mut values: IndexMap<String, String> = IndexMap::new();
    for (name, value) in headers {
        http::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("Headers.append: \"{name}\" is an invalid header name."))?;
        let value = value.trim_matches([' ', '\t', '\r', '\n']);
        if value.contains(['\r', '\n', '\0']) {
            return Err(format!(
                "Headers.append: \"{value}\" is an invalid header value."
            ));
        }
        values
            .entry(name.to_ascii_lowercase())
            .and_modify(|previous| {
                previous.push_str(", ");
                previous.push_str(value);
            })
            .or_insert_with(|| value.into());
    }
    Ok(values)
}
fn chunked(url: &Url) -> bool {
    url.path().contains(":streamGenerateContent")
}
pub fn header_pairs(
    url: &Url,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<Vec<(String, String)>, String> {
    let headers = header_values(headers)?;
    let host = url.host_str().unwrap_or_default();
    let host = url
        .port()
        .map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"));
    let mut result = vec![
        ("Host".into(), host),
        (
            "User-Agent".into(),
            headers
                .get("user-agent")
                .cloned()
                .unwrap_or_else(default_user_agent),
        ),
    ];
    result.push(if chunked(url) {
        ("Transfer-Encoding".into(), "chunked".into())
    } else {
        ("Content-Length".into(), body.len().to_string())
    });
    if let Some(value) = headers
        .get("authorization")
        .filter(|value| !value.is_empty())
    {
        result.push(("Authorization".into(), value.clone()));
    }
    result.push((
        "Content-Type".into(),
        headers
            .get("content-type")
            .cloned()
            .unwrap_or_else(|| "application/json".into()),
    ));
    result.push((
        "Accept-Encoding".into(),
        headers
            .get("accept-encoding")
            .cloned()
            .unwrap_or_else(|| "gzip".into()),
    ));
    Ok(result)
}
pub fn serialize_request(
    url: &Url,
    method: Option<&str>,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<Vec<u8>, String> {
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().into(),
    };
    let headers = header_pairs(url, headers, body)?
        .into_iter()
        .map(|(key, value)| format!("{key}: {value}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let mut result = format!(
        "{} {path} HTTP/1.1\r\n{headers}\r\n\r\n",
        method.unwrap_or("POST")
    )
    .into_bytes();
    if !body.is_empty() {
        if chunked(url) {
            result.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
        }
        result.extend_from_slice(body);
        if chunked(url) {
            result.extend_from_slice(b"\r\n0\r\n\r\n");
        }
    }
    Ok(result)
}
/// Resolve against the request's environment snapshot; never switch process variables.
pub fn https_proxy(url: &Url, environment: &HashMap<String, String>) -> Option<Url> {
    if url.scheme() != "https" {
        return None;
    }
    let get = |names: &[&str]| {
        names
            .iter()
            .find_map(|key| environment.get(*key).filter(|value| !value.is_empty()))
    };
    if let Some(no_proxy) = get(&["NO_PROXY", "no_proxy"]) {
        let host = url.host_str().unwrap_or_default().to_lowercase();
        if no_proxy
            .split(',')
            .map(|entry| trim(entry).to_lowercase())
            .filter(|entry| !entry.is_empty())
            .any(|entry| {
                entry == "*"
                    || if entry.starts_with('.') {
                        host.ends_with(&entry)
                    } else {
                        host == entry || host.ends_with(&format!(".{entry}"))
                    }
            })
        {
            return None;
        }
    }
    get(&["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"])
        .and_then(|value| Url::parse(value).ok())
}

#[derive(Debug)]
pub struct ResponseHead {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub chunked: bool,
    pub gzip: bool,
    pub content_length: Option<f64>,
}
fn integer_prefix(input: &str, radix: u32) -> Option<f64> {
    let mut input = trim(input);
    let negative = input.starts_with('-');
    if input.starts_with(['+', '-']) {
        input = &input[1..];
    }
    if radix == 16 {
        input = input
            .strip_prefix("0x")
            .or_else(|| input.strip_prefix("0X"))
            .unwrap_or(input);
    }
    let end = input
        .bytes()
        .take_while(|value| (*value as char).is_digit(radix))
        .count();
    if end == 0 {
        return None;
    }
    let digits = &input[..end];
    let value = if radix == 10 {
        digits.parse::<f64>().ok()?
    } else {
        u128::from_str_radix(digits, radix)
            .map(|value| value as f64)
            .unwrap_or_else(|_| {
                digits.bytes().fold(0.0, |total, digit| {
                    total * f64::from(radix) + f64::from((digit as char).to_digit(radix).unwrap())
                })
            })
    };
    (value.is_finite()).then_some(if negative { -value } else { value })
}
pub fn parse_response_head(head: &str) -> Result<ResponseHead, String> {
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    static STATUS: OnceLock<Regex> = OnceLock::new();
    let pattern = STATUS.get_or_init(|| Regex::new(r"^HTTP/1\.[01][\x09-\x0d \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]+([0-9]{3})[\x09-\x0d \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]*([^\r\n\u{2028}\u{2029}]*)$").unwrap());
    let matched = pattern
        .captures(status_line)
        .ok_or_else(|| format!("Invalid HTTP response: {status_line}"))?;
    let mut result = ResponseHead {
        status: matched[1].parse().unwrap(),
        status_text: matched[2].into(),
        headers: Vec::new(),
        chunked: false,
        gzip: false,
        content_length: None,
    };
    let mut headers = Vec::new();
    let mut cookies = Vec::new();
    for line in lines {
        let Some((key, value)) = line.split_once(':').filter(|(key, _)| !key.is_empty()) else {
            continue;
        };
        let value = trim(value);
        let lower = key.to_ascii_lowercase();
        let lower_value = value.to_ascii_lowercase();
        if lower == "transfer-encoding" && lower_value.contains("chunked") {
            result.chunked = true;
            continue;
        }
        if lower == "content-encoding" && lower_value.contains("gzip") {
            result.gzip = true;
            continue;
        }
        if lower == "content-length" {
            if let Some(length) = integer_prefix(value, 10).filter(|length| *length >= 0.0) {
                result.content_length = Some(length);
            }
            if result.gzip {
                continue;
            }
        }
        if lower == "set-cookie" {
            let validated = header_values(&[(key.to_owned(), value.to_owned())])?;
            cookies.push((lower, validated["set-cookie"].clone()));
        } else {
            headers.push((key.to_owned(), value.to_owned()));
        }
    }
    result.headers = header_values(&headers)?.into_iter().collect();
    result.headers.extend(cookies);
    result.headers.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}
