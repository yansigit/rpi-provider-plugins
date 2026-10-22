use rpi_multiprovider::lift::{Failover, Response, StreamKind};
use rpi_multiprovider::services::{Classifier, Options, Service};
use rpi_multiprovider::types::{Account, AttemptFailure, Disposition};
use rpi_multiprovider::virtual_provider::{
    capture_virtual_model_template, create_virtual_integrations, heal_virtual_templates, virtual_scheduler_id,
    Attempt, AmbientAuth, AmbientResolution, IntegrationOptions, Pending, VirtualHost, VirtualIntegration,
    VirtualProvider, VirtualProviderConfig,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

fn normalize_numbers(value: Value) -> Value {
    match value {
        Value::Array(values) => values.into_iter().map(normalize_numbers).collect(),
        Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| (key, normalize_numbers(value)))
            .collect(),
        Value::Number(number) => json!(number.as_f64().unwrap()),
        value => value,
    }
}

fn with_now(value: Value, now: f64) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(|value| with_now(value, now)).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let value = if key == "timestamp" && value == "$NOW" { json!(now) } else { with_now(value, now) };
                    (key, value)
                })
                .collect(),
        ),
        value => value,
    }
}

fn failure_json(value: &AttemptFailure) -> Value {
    let mut result = json!({"message": value.message, "outputStarted": value.output_started});
    if let Some(status) = value.status {
        result["status"] = json!(status);
    }
    for (key, value) in [
        ("headers", &value.headers),
        ("assistantMessage", &value.assistant_message),
        ("cause", &value.cause),
    ] {
        if let Some(value) = value {
            result[key] = value.clone();
        }
    }
    result
}

fn disposition_json(result: &Result<Option<Disposition>, String>) -> Value {
    match result {
        Ok(Some(disposition)) => serde_json::to_value(disposition).unwrap(),
        Ok(None) => Value::Null,
        Err(error) => json!({"error": error}),
    }
}

fn account_json(account: &Account) -> Value {
    let mut result = json!({
        "id": account.id,
        "label": account.label,
        "authKind": account.auth_kind,
        "credentialRef": account.credential_ref,
    });
    if let Some(weight) = account.weight {
        result["weight"] = json!(weight);
    }
    if let Some(metadata) = &account.metadata {
        result["metadata"] = metadata.clone();
    }
    result
}

fn template_key(provider_id: &str, model_id: &str) -> String {
    format!("{provider_id}::{model_id}")
}

struct Fixture {
    input: Value,
    trace: Mutex<Vec<Value>>,
    clock: Arc<Mutex<f64>>,
    affinity: Mutex<String>,
    backing: Mutex<HashMap<String, Vec<Value>>>,
    cursors: Mutex<HashMap<String, usize>>,
    response: Mutex<Option<Response>>,
    events: Mutex<Vec<Value>>,
    active: Mutex<Option<CancellationToken>>,
}

impl Fixture {
    fn new(input: Value, clock: Arc<Mutex<f64>>) -> Self {
        let backing = input["backing"]
            .as_object()
            .map(|models| {
                models
                    .iter()
                    .map(|(provider, models)| (provider.clone(), models.as_array().cloned().unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            input,
            trace: Mutex::new(Vec::new()),
            clock,
            affinity: Mutex::new("session-1".into()),
            backing: Mutex::new(backing),
            cursors: Mutex::new(HashMap::new()),
            response: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            active: Mutex::new(None),
        }
    }

    fn record(&self, entry: Value) {
        self.trace.lock().unwrap().push(entry);
    }

    fn now(&self) -> f64 {
        *self.clock.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl VirtualHost for Fixture {
    type Stream = futures_util::stream::Iter<std::vec::IntoIter<Result<Value, String>>>;

    fn now(&self) -> f64 {
        Fixture::now(self)
    }

    fn affinity_key(&self) -> Result<String, String> {
        let key = self.affinity.lock().unwrap().clone();
        self.record(json!({"affinity": key}));
        Ok(key)
    }

    fn backing_models(&self, provider_id: &str) -> Option<Vec<Value>> {
        let models = self.backing.lock().unwrap().get(provider_id).cloned();
        self.record(json!({"lookup": provider_id, "found": models.is_some()}));
        models
    }

    fn is_backend_configured(&self, provider_id: &str) -> bool {
        let configured = self.input["isBackendConfigured"][provider_id].as_bool().unwrap_or(true);
        self.record(json!({"configured": provider_id, "result": configured}));
        configured
    }

    async fn resolve_ambient_auth(
        &self,
        provider_id: &str,
        model: &Value,
        _signal: &CancellationToken,
    ) -> Result<AmbientResolution, String> {
        self.record(json!({"ambient": {"providerId": provider_id, "model": model["id"]}}));
        let scripted = &self.input["ambient"][provider_id];
        if let Some(error) = scripted["throw"].as_str() {
            return Err(error.into());
        }
        if scripted.is_null() {
            return Ok(AmbientResolution::Resolved(AmbientAuth {
                api_key: Some(format!("ambient-{provider_id}")),
                ..AmbientAuth::default()
            }));
        }
        if scripted["ok"] == false {
            return Ok(AmbientResolution::Unresolved {
                error: scripted["error"].as_str().unwrap_or_default().into(),
            });
        }
        Ok(AmbientResolution::Resolved(AmbientAuth {
            api_key: scripted["apiKey"].as_str().map(str::to_owned),
            headers: scripted["headers"].as_object().cloned(),
            base_url: scripted["baseUrl"].as_str().map(str::to_owned),
            env: scripted["env"].as_object().cloned(),
        }))
    }

    async fn start(&self, attempt: Attempt) -> Result<Self::Stream, String> {
        *self.response.lock().unwrap() = None;
        let kind = match attempt.kind {
            StreamKind::Full => "stream",
            StreamKind::Simple => "streamSimple",
        };
        self.record(json!({"send": {
            "provider": attempt.provider_id,
            "kind": kind,
            "model": attempt.model,
            "context": attempt.context,
            "options": attempt.options,
        }}));
        let queue = self.input["scripts"][attempt.provider_id.as_str()].as_array().cloned();
        let index = {
            let mut cursors = self.cursors.lock().unwrap();
            let cursor = cursors.entry(attempt.provider_id.clone()).or_insert(0);
            let index = *cursor;
            *cursor += 1;
            index
        };
        let Some(script) = queue.as_ref().and_then(|queue| queue.get(index)).cloned() else {
            return Err(format!("fixture script exhausted: {}", attempt.provider_id));
        };
        if script["abort"] == true {
            if let Some(token) = self.active.lock().unwrap().clone() {
                token.cancel();
            }
        }
        if let Some(error) = script["throw"].as_str() {
            return Err(error.into());
        }
        if let Some(response) = script.get("response") {
            self.record(json!({"onResponse": {"status": response["status"], "headers": response["headers"], "model": attempt.model}}));
            *self.response.lock().unwrap() = Some(Response {
                status: response["status"].as_u64().unwrap() as u16,
                headers: response["headers"].clone(),
            });
            if attempt.on_response {
                self.record(json!({"userOnResponse": {"status": response["status"], "headers": response["headers"], "model": attempt.model}}));
            }
        }
        let now = self.now();
        let events = script["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|event| match event["throw"].as_str() {
                Some(error) => Err(error.to_string()),
                None => Ok(with_now(event, now)),
            })
            .collect::<Vec<_>>();
        Ok(futures_util::stream::iter(events))
    }

    fn response(&self) -> Option<Response> {
        self.response.lock().unwrap().clone()
    }

    async fn emit(&self, event: Value) {
        self.events.lock().unwrap().push(event);
    }

    async fn pause(&self) {
        self.record(json!({"pause": 250}));
        *self.clock.lock().unwrap() += 250.0;
        if self.input["abortOnPause"] == true {
            if let Some(token) = self.active.lock().unwrap().clone() {
                token.cancel();
            }
        }
    }

    async fn failover(&self, info: Failover<'_>) -> Result<bool, String> {
        let Some(mode) = self.input["onFailover"].as_str() else {
            return Ok(false);
        };
        self.record(json!({"failover": {
            "providerId": info.provider_id,
            "fromAccountId": info.from_account_id,
            "failure": failure_json(info.failure),
            "errorsOnAccount": info.errors_on_account,
        }}));
        match mode {
            "throw" => Err("fixture failover failed".into()),
            "surface" => Ok(true),
            _ => Ok(false),
        }
    }
}

struct Slot<'a> {
    pending: Pending<'a, Fixture>,
    token: Option<CancellationToken>,
}

fn kind_of(op: &Value) -> StreamKind {
    if op["kind"] == "streamSimple" {
        StreamKind::Simple
    } else {
        StreamKind::Full
    }
}

async fn run_case(input: &Value) -> Value {
    let clock = Arc::new(Mutex::new(1_700_000_000_000.0));
    let fixture = Arc::new(Fixture::new(input.clone(), clock.clone()));

    let mut settings = json!({});
    if let Some(value) = input["service"].get("errorsBeforeSwitch") {
        settings["errorsBeforeSwitch"] = value.clone();
    }
    let service_clock = clock.clone();
    let random = fixture.clone();
    let service = Service::new(Options {
        now: Arc::new(move || *service_clock.lock().unwrap()),
        random_id: Arc::new(|| "lease".into()),
        random_int: Arc::new(move |max| {
            random.record(json!({"randomInt": max}));
            Ok(0)
        }),
        settings: serde_json::from_value(settings).unwrap(),
        affinity: input["service"]["affinity"].as_bool().unwrap_or(true),
        ..Options::default()
    });

    let config: VirtualProviderConfig = serde_json::from_value(input["config"].clone()).unwrap();
    let mut integration_options = IntegrationOptions {
        max_account_attempts: input["integrationOptions"]["maxAccountAttempts"].as_f64(),
        ..IntegrationOptions::default()
    };
    if let Some(labels) = input["integrationOptions"]["labels"].as_object().cloned() {
        integration_options.provider_label = Some(Arc::new(move |id: &str| {
            labels.get(id).and_then(Value::as_str).map(str::to_owned)
        }));
    }
    let mut integrations: Vec<VirtualIntegration> = create_virtual_integrations(&config, &integration_options);
    for integration in &mut integrations {
        let original = integration.registration.classify_failure.clone();
        let host = fixture.clone();
        let wrapped: Arc<Classifier> = Arc::new(move |failure: &AttemptFailure, account: &Account| {
            let result = match &original {
                Some(classify) => classify(failure, account),
                None => Ok(None),
            };
            host.record(json!({
                "classify": {"failure": failure_json(failure), "account": account.id},
                "result": disposition_json(&result),
            }));
            result
        });
        integration.registration.classify_failure = Some(wrapped);
    }

    let provider = VirtualProvider::new(
        service.clone(),
        config.clone(),
        fixture.clone(),
        input["providerOptions"]["maxAccountAttempts"].as_f64(),
    );
    let scheduler = virtual_scheduler_id(&config.id, "ultra");
    let templates: Map<String, Value> = input["templates"].as_object().cloned().unwrap_or_default();
    let mut slots: HashMap<String, Slot<'_>> = HashMap::new();
    let mut outputs: Vec<Value> = Vec::new();

    for (index, op) in input["ops"].as_array().unwrap().iter().enumerate() {
        fixture.record(json!({"begin": index}));
        let output = match op["op"].as_str().unwrap() {
            "register" => {
                let mut errors = Vec::new();
                for integration in &integrations {
                    if let Err(error) = service.register(integration.to_registration()) {
                        errors.push(error.to_string());
                    }
                }
                json!({"errors": errors})
            }
            "snapshot" => service.snapshot().await.unwrap(),
            "models" => json!(provider.models()),
            "auth" => provider.resolve_auth().unwrap_or(Value::Null),
            "integrations" => {
                let mut output = Vec::new();
                for integration in &integrations {
                    let accounts = (integration.registration.accounts)().await.unwrap();
                    output.push(json!({
                        "id": integration.registration.id,
                        "label": integration.registration.label,
                        "selectionBias": integration.registration.selection_bias,
                        "maxAccountAttempts": integration.max_account_attempts,
                        "accounts": accounts.iter().map(account_json).collect::<Vec<_>>(),
                    }));
                }
                json!(output)
            }
            "capture" => {
                let provider_id = op["provider"].as_str().unwrap();
                let model_id = op["model"].as_str().unwrap();
                let models = fixture.backing.lock().unwrap().get(provider_id).cloned().unwrap_or_default();
                let model = models.iter().find(|item| item["id"] == model_id).unwrap();
                capture_virtual_model_template(model)
            }
            "heal" => {
                let opts: Map<String, Value> = match op.get("templates") {
                    Some(value) => value.as_object().cloned().unwrap_or_default(),
                    None => templates.clone(),
                };
                let healed = heal_virtual_templates(&config, |provider_id, model_id| {
                    fixture.record(json!({"resolveTemplate": {"providerId": provider_id, "modelId": model_id}}));
                    opts.get(&template_key(provider_id, model_id)).cloned()
                });
                match healed {
                    Some(config) => serde_json::to_value(config).unwrap(),
                    None => Value::Null,
                }
            }
            "setAffinity" => {
                *fixture.affinity.lock().unwrap() = op["key"].as_str().unwrap().into();
                Value::Null
            }
            "setSettings" => {
                service.update_settings(&op["settings"]).unwrap();
                Value::Null
            }
            "setBacking" => {
                let provider_id = op["provider"].as_str().unwrap().to_string();
                let mut backing = fixture.backing.lock().unwrap();
                match op["models"].as_array() {
                    Some(models) => backing.insert(provider_id, models.clone()),
                    None => backing.remove(&provider_id),
                };
                Value::Null
            }
            "advance" => {
                *clock.lock().unwrap() += op["ms"].as_f64().unwrap();
                Value::Null
            }
            "abort" => {
                if let Some(token) = slots.get(op["slot"].as_str().unwrap()).and_then(|slot| slot.token.clone()) {
                    token.cancel();
                }
                Value::Null
            }
            "affinity" => match service.affinity(&scheduler, op["key"].as_str().unwrap()).unwrap() {
                Some(pin) => serde_json::to_value(pin).unwrap(),
                None => Value::Null,
            },
            "pin" => match service
                .pin_account(&scheduler, op["key"].as_str().unwrap(), op["account"].as_str().unwrap())
                .await
            {
                Ok(()) => json!({}),
                Err(error) => json!({"error": error.to_string()}),
            },
            "classify" => {
                let index = op["index"].as_u64().unwrap_or(0) as usize;
                let failure: AttemptFailure = serde_json::from_value(op["failure"].clone()).unwrap();
                let account: Account = serde_json::from_value(json!({
                    "id": "classify-probe", "label": "", "authKind": "custom", "credentialRef": null
                }))
                .unwrap();
                let classify = integrations[index].registration.classify_failure.clone().unwrap();
                disposition_json(&classify(&failure, &account))
            }
            "open" => {
                let model = provider.models().into_iter().find(|item| item["id"] == op["model"]).unwrap();
                if let Some(key) = op["affinityKey"].as_str() {
                    *fixture.affinity.lock().unwrap() = key.into();
                }
                let token = (op["signal"] == true).then(CancellationToken::new);
                *fixture.active.lock().unwrap() = token.clone();
                let options: Map<String, Value> = op["options"].as_object().cloned().unwrap_or_default();
                let context = op.get("context").cloned().unwrap_or_else(|| json!({"messages": []}));
                let pending = provider.open(
                    kind_of(op),
                    &model,
                    context,
                    options,
                    token.clone().unwrap_or_default(),
                    op["onResponse"] == true,
                );
                slots.insert(op["slot"].as_str().unwrap().into(), Slot { pending, token });
                Value::Null
            }
            "drain" => {
                let slot = slots.remove(op["slot"].as_str().unwrap()).unwrap();
                slot.pending.drive().await;
                json!({"events": std::mem::take(&mut *fixture.events.lock().unwrap())})
            }
            "stream" => {
                let model = provider.models().into_iter().find(|item| item["id"] == op["model"]).unwrap();
                if let Some(key) = op["affinityKey"].as_str() {
                    *fixture.affinity.lock().unwrap() = key.into();
                }
                let token = (op["signal"] == true).then(CancellationToken::new);
                *fixture.active.lock().unwrap() = token.clone();
                let options: Map<String, Value> = op["options"].as_object().cloned().unwrap_or_default();
                let context = op.get("context").cloned().unwrap_or_else(|| json!({"messages": []}));
                let pending = provider.open(
                    kind_of(op),
                    &model,
                    context,
                    options,
                    token.unwrap_or_default(),
                    op["onResponse"] == true,
                );
                pending.drive().await;
                json!({"events": std::mem::take(&mut *fixture.events.lock().unwrap())})
            }
            other => panic!("unknown op {other}"),
        };
        outputs.push(output);
    }

    json!({
        "ops": outputs,
        "trace": *fixture.trace.lock().unwrap(),
        "now": *clock.lock().unwrap(),
    })
}

#[tokio::test]
async fn virtual_provider_matches_upstream_traces() {
    let reference: Value = serde_json::from_str(include_str!("virtual-reference.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert!(cases.len() >= 18, "fixture must cover the 18 upstream virtual.spec cases");
    for case in cases {
        let actual = run_case(&case["input"]).await;
        assert_eq!(
            normalize_numbers(actual),
            normalize_numbers(case["result"].clone()),
            "{}",
            case["name"]
        );
    }
}
