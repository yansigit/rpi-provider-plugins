use rpi_antigravity_auth::metadata::{self, SessionContext, SessionStore};
use serde_json::{json, Value};

fn equal(actual: Value, expected: &Value) {
    assert_eq!(&actual, expected);
    assert_eq!(
        serde_json::to_string(&actual).unwrap(),
        serde_json::to_string(expected).unwrap(),
        "field order"
    );
}
#[test]
fn metadata_and_generation_match_pinned_source() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/metadata.json")).unwrap();
    for case in fixture["fnv"].as_array().unwrap() {
        equal(
            json!(metadata::fnv1a64_signed(case["input"].as_str().unwrap())),
            &case["result"],
        );
    }
    for case in fixture["steps"].as_array().unwrap() {
        equal(
            json!(metadata::count_steps(
                &case["payload"],
                serde_json::from_value(case["mode"].clone()).unwrap()
            )),
            &case["result"],
        );
    }
    for case in fixture["calls"].as_array().unwrap() {
        let mut session: SessionContext = serde_json::from_value(case["before"].clone()).unwrap();
        equal(
            metadata::build(
                &mut session,
                &case["payload"],
                case["model"].as_str().unwrap(),
                1784285195116,
                serde_json::from_value(case["mode"].clone()).unwrap(),
            ),
            &case["result"],
        );
        equal(serde_json::to_value(session).unwrap(), &case["after"]);
    }
    for case in fixture["generations"].as_array().unwrap() {
        let mut config = case["config"].clone();
        metadata::apply_generation(case["model"].as_str().unwrap(), &mut config);
        equal(config, &case["result"]);
    }
    for case in fixture["finalizations"].as_array().unwrap() {
        let mut request = case["before"].clone();
        let mut session = SessionContext::new("", "conversation".into(), "trajectory".into());
        let id = metadata::finalize(
            &mut request,
            case["model"].as_str().unwrap(),
            &mut session,
            1000,
        );
        equal(json!(id), &case["result"]);
        equal(request, &case["request"]);
        equal(serde_json::to_value(session).unwrap(), &case["session"]);
    }
}

#[test]
fn session_lifetime_and_ids_match_original() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/metadata.json")).unwrap();
    for trace in fixture["stores"].as_array().unwrap() {
        let mut store = SessionStore::with_limits(
            "file:///workspace".into(),
            trace["limits"]["ttlMs"].as_i64().unwrap(),
            trace["limits"]["maxEntries"].as_u64().unwrap() as usize,
        );
        for op in trace["operations"].as_array().unwrap() {
            let key = op["key"].as_str().unwrap();
            let mut ids = op["ids"].as_array().unwrap().iter();
            let mut next_id = || {
                ids.next()
                    .expect("unexpected ID allocation")
                    .as_str()
                    .unwrap()
                    .to_owned()
            };
            let now = op["now"].as_i64().unwrap();
            match op["op"].as_str().unwrap() {
                "getOrCreate" => equal(
                    serde_json::to_value(store.get_or_create(key, now, &mut next_id)).unwrap(),
                    &op["result"],
                ),
                "beginRequest" => {
                    let (session, timestamp) = store.begin_request(key, now, &mut next_id);
                    equal(
                        json!({"session":session,"timestamp":timestamp}),
                        &op["result"],
                    );
                }
                "completeExecution" => store.complete_execution(key, &mut next_id),
                "delete" => store.delete(key),
                "clear" => store.clear(),
                other => panic!("unrecognized store operation {other}"),
            }
            assert_eq!(ids.next(), None, "unused ID allocation: {op}");
            assert_eq!(store.len(), op["size"].as_u64().unwrap() as usize, "{op}");
            for pair in op["has"].as_array().unwrap() {
                assert_eq!(
                    store.has(pair[0].as_str().unwrap()),
                    pair[1].as_bool().unwrap(),
                    "{op}"
                );
            }
        }
    }
}

#[test]
fn physical_request_bodies_match_original_provider() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/stream.json")).unwrap();
    let now = fixture["now"].as_i64().unwrap();
    let mut sessions = SessionStore::new(String::new());
    for case in fixture["cases"].as_array().unwrap() {
        let mut ids = case["ids"].as_array().unwrap().iter();
        let result = rpi_antigravity_auth::request::prepare(
            &case["model"],
            &case["context"],
            &case["options"],
            &mut sessions,
            now,
            &mut || {
                ids.next()
                    .expect("unexpected request ID")
                    .as_str()
                    .unwrap()
                    .into()
            },
        );
        if case["requests"].as_array().unwrap().is_empty() {
            assert_eq!(
                result.err().unwrap(),
                case["result"]["errorMessage"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(ids.next(), None, "refused request allocated IDs");
        } else {
            let request = result.unwrap();
            assert_eq!(
                request.body("fixture-project"),
                case["requests"][0]["body"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            if matches!(
                case["result"]["stopReason"].as_str(),
                Some("stop" | "length")
            ) {
                let execution = ids
                    .next_back()
                    .expect("execution ID")
                    .as_str()
                    .unwrap()
                    .to_owned();
                sessions.complete_execution(
                    &metadata::session_key(&case["context"], &case["options"]),
                    &mut || execution.clone(),
                );
            }
        }
    }
}
