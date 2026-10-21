use rpi_antigravity_auth::{
    metadata::{self, SessionStore},
    request,
    sse::DecoderState,
    stream::{EventSink, Flow, State},
};
use serde_json::Value;

#[derive(Default)]
struct Events(Vec<Value>);
impl EventSink for Events {
    async fn emit(&mut self, event: Value) -> Result<(), String> {
        self.0.push(event);
        Ok(())
    }
}
#[test]
fn requests_events_partials_usage_and_terminal_outcomes_match_original() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let fixture: Value = serde_json::from_str(include_str!("fixtures/stream.json")).unwrap();
        let now = fixture["now"].as_i64().unwrap();
        let mut sessions = SessionStore::new(String::new());
        for case in fixture["cases"].as_array().unwrap() {
            let mut ids = case["ids"].as_array().unwrap().iter();
            let mut next_id = || {
                ids.next()
                    .expect("unexpected ID allocation")
                    .as_str()
                    .unwrap()
                    .to_owned()
            };
            let mut state = State::new(&case["model"], now);
            let mut events = Events::default();
            state.start(&mut events).await.unwrap();
            let result = async {
                let prepared = request::prepare(
                    &case["model"],
                    &case["context"],
                    &case["options"],
                    &mut sessions,
                    now,
                    &mut next_id,
                )?;
                assert_eq!(
                    prepared.body("fixture-project"),
                    case["requests"][0]["body"].as_str().unwrap(),
                    "{}",
                    case["name"]
                );
                let status = case["status"].as_u64().unwrap();
                if !(200..300).contains(&status) {
                    return Err(format!(
                        "Antigravity request failed: HTTP {status} {}",
                        case["chunks"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|chunk| chunk.as_str().unwrap())
                            .collect::<String>()
                    ));
                }
                let mut decoder = DecoderState::default();
                let mut complete = false;
                for bytes in case["chunks"].as_array().unwrap() {
                    decoder.push(bytes.as_str().unwrap().as_bytes());
                    while let Some(chunk) = decoder.next_chunk(false) {
                        if state.chunk(&chunk, &mut next_id, &mut events).await? == Flow::Complete {
                            complete = true;
                            break;
                        }
                    }
                    if complete {
                        break;
                    }
                }
                if !complete {
                    while let Some(chunk) = decoder.next_chunk(true) {
                        if state.chunk(&chunk, &mut next_id, &mut events).await? == Flow::Complete {
                            break;
                        }
                    }
                }
                state.finish(false, 0, &mut events).await?;
                if matches!(state.output["stopReason"].as_str(), Some("stop" | "length")) {
                    sessions.complete_execution(
                        &metadata::session_key(&case["context"], &case["options"]),
                        &mut next_id,
                    );
                }
                Ok::<_, String>(())
            }
            .await;
            if let Err(error) = result {
                state
                    .fail(
                        &error,
                        false,
                        0,
                        &["fixture-marker".into(), "fixture-access".into()],
                        &["fixture-project".into()],
                        &mut events,
                    )
                    .await
                    .unwrap();
            }
            assert_eq!(ids.next(), None, "unused reference ID: {}", case["name"]);
            assert_eq!(state.output, case["result"], "terminal: {}", case["name"]);
            assert_eq!(
                Value::Array(events.0),
                case["events"],
                "events: {}",
                case["name"]
            );
        }
    });
}
