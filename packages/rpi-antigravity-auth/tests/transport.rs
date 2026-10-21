use rpi_antigravity_auth::transport;
use serde_json::{json, Value};
use url::Url;

#[test]
fn body_framing_and_incremental_gzip_match_original() {
    use transport::body::{Framer, Inflater};
    let fixture: Value = serde_json::from_str(include_str!("fixtures/transport.json")).unwrap();
    for case in fixture["responses"].as_array().unwrap() {
        let head = transport::parse_response_head(case["head"].as_str().unwrap()).unwrap();
        let wire: Vec<u8> = serde_json::from_value(case["wireBody"].clone()).unwrap();
        for width in [1, 7, wire.len().max(1)] {
            let mut framed = Vec::new();
            let mut framer = Framer::new(&head);
            for bytes in wire.chunks(width) {
                framer.push(bytes);
                while let Some(bytes) = framer.next(false).unwrap() {
                    framed.push(bytes);
                }
            }
            while let Some(bytes) = framer.next(true).unwrap() {
                framed.push(bytes);
            }
            assert!(framer.is_complete());
            let mut decoded = Vec::new();
            if head.gzip {
                let mut decoder = Inflater::default();
                for bytes in &framed {
                    let mut position = 0;
                    while position < bytes.len() {
                        let (consumed, output) = decoder.push(&bytes[position..]).unwrap();
                        assert!(consumed > 0 || !output.is_empty());
                        assert!(output.len() <= 16384);
                        position += consumed;
                        decoded.extend(output);
                    }
                    loop {
                        let (done, output) = decoder.drain(false).unwrap();
                        assert!(output.len() <= 16384);
                        decoded.extend(output);
                        if done {
                            break;
                        }
                    }
                }
                loop {
                    let (done, output) = decoder.drain(true).unwrap();
                    assert!(output.len() <= 16384);
                    decoded.extend(output);
                    if done {
                        break;
                    }
                }
            } else {
                decoded = framed.concat();
            }
            assert_eq!(
                decoded,
                case["body"].as_str().unwrap().as_bytes(),
                "head: {} width: {width}",
                case["head"]
            );
        }
    }
}

#[test]
fn headers_and_serialized_requests_match_original_transport() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/transport.json")).unwrap();
    for case in fixture["agents"].as_array().unwrap() {
        assert_eq!(
            transport::user_agent(
                case["version"].as_str().unwrap(),
                case["platform"].as_str().unwrap(),
                case["arch"].as_str().unwrap(),
                case["auth"].as_str().unwrap()
            ),
            case["result"].as_str().unwrap()
        );
    }
    for case in fixture["responses"].as_array().unwrap() {
        let head = transport::parse_response_head(case["head"].as_str().unwrap()).unwrap();
        assert_eq!(json!(head.headers), case["headers"], "{case}");
        assert_eq!(json!(head.status), case["status"], "{case}");
        assert_eq!(
            head.status_text,
            case["statusText"].as_str().unwrap(),
            "{case}"
        );
    }
    for case in fixture["headerInputs"].as_array().unwrap() {
        let mut headers: Vec<(String, String)> =
            serde_json::from_value(case["headers"].clone()).unwrap();
        headers.push((
            "User-Agent".into(),
            transport::user_agent(
                "1.3.5",
                fixture["platform"].as_str().unwrap(),
                fixture["arch"].as_str().unwrap(),
                "consumer",
            ),
        ));
        let result = transport::header_pairs(
            &Url::parse("https://service.example.test/json").unwrap(),
            &headers,
            &[],
        );
        if let Some(error) = case["error"].as_str() {
            assert_eq!(result.err().unwrap(), error, "{case}");
        } else {
            assert_eq!(json!(result.unwrap()), case["result"], "{case}");
        }
    }
    for case in fixture["cases"].as_array().unwrap() {
        let url = Url::parse(case["url"].as_str().unwrap()).unwrap();
        let mut headers = case["init"]
            .get("headers")
            .map(|headers| {
                serde_json::from_value::<Vec<(String, String)>>(headers.clone()).unwrap()
            })
            .unwrap_or_default();
        // The original's platform is an injected input when testing its captured wire bytes.
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
        {
            headers.push((
                "User-Agent".into(),
                transport::user_agent(
                    "1.3.5",
                    fixture["platform"].as_str().unwrap(),
                    fixture["arch"].as_str().unwrap(),
                    "consumer",
                ),
            ));
        }
        let body = case["init"]["body"].as_str().unwrap_or_default().as_bytes();
        assert_eq!(
            json!(transport::header_pairs(&url, &headers, body).unwrap()),
            case["pairs"],
            "{case}"
        );
        let bytes =
            transport::serialize_request(&url, case["init"]["method"].as_str(), &headers, body)
                .unwrap();
        let send = case["writes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|write| write["stage"] == "tls")
            .unwrap();
        assert_eq!(json!(bytes), send["bytes"], "{case}");
        let environment = serde_json::from_value(case["environment"].clone()).unwrap();
        let proxy = transport::https_proxy(&url, &environment);
        let expected = case["connections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|connection| connection["stage"] == "proxy");
        match (proxy, expected) {
            (Some(proxy), Some(expected)) => {
                assert_eq!(
                    proxy.host_str().unwrap(),
                    expected["host"].as_str().unwrap(),
                    "{case}"
                );
                assert_eq!(
                    u64::from(proxy.port().unwrap_or(8080)),
                    expected["port"].as_u64().unwrap(),
                    "{case}"
                );
            }
            (None, None) => {}
            other => panic!("proxy mismatch: {other:?}: {case}"),
        }
    }
}
