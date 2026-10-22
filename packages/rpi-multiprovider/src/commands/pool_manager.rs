//! `extensions/pool-manager.ts`, projected onto native dialog controls, with
//! the pool manager callbacks from `extensions/multiprovider.ts` (the
//! `/multilogin` handler block) reading and writing the account store through
//! [`Port`].
//!
//! Upstream shows one `ctx.ui.custom` component whose `SettingsList` swaps
//! between the root rows and nested submenus. The port shows one native control
//! per screen: a `settings` control for the root and each `SectionSubmenu`, a
//! `select` control for each `SelectSubmenu`, and an `input` control for each
//! string/integer submenu. A change persists, reloads the state and rebuilds the
//! root screen, exactly like `component.refresh(selectAfterFor(id))`.
use super::port::{notify, PoolAction, PoolBuffer, PoolMethod, Port, ProviderInfo};
use super::settings::{self, SettingItem, BOOLEANS};
use super::Ui;
use crate::types::trim;
use serde_json::{json, Map, Value};

pub const UPSTREAM_DEFAULT_LABEL: &str = "Pi default";
const POLICIES: [&str; 4] = [
    "round-robin",
    "weighted-round-robin",
    "least-inflight",
    "priority",
];
const REMOVE_SENTINEL: &str = "__remove__";
const ADD_SENTINEL_PREFIX: &str = "__add__:";
const REAUTH_SENTINEL_PREFIX: &str = "__reauth__:";
const RATE_LIMIT_OPTIONS: [f64; 6] = [15_000.0, 30_000.0, 60_000.0, 120_000.0, 300_000.0, 600_000.0];
const QUOTA_OPTIONS: [f64; 5] = [300_000.0, 900_000.0, 1_800_000.0, 3_600_000.0, 7_200_000.0];
const AUTH_OPTIONS: [f64; 4] = [60_000.0, 300_000.0, 900_000.0, 1_800_000.0];
const TRANSIENT_OPTIONS: [f64; 7] = [100.0, 250.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 15_000.0];
const MAX_COOLDOWN_OPTIONS: [f64; 6] = [300_000.0, 600_000.0, 1_800_000.0, 3_600_000.0, 7_200_000.0, 21_600_000.0];
const COUNT_OPTIONS: [f64; 4] = [1.0, 2.0, 3.0, 5.0];

pub const SCHEDULER_SETTING_KEYS: [&str; 6] = [
    "rateLimitCooldownMs",
    "quotaCooldownMs",
    "authCooldownMs",
    "transientBaseCooldownMs",
    "maxCooldownMs",
    "errorsBeforeSwitch",
];
pub const SCHEDULER_DEFAULTS: [(&str, f64); 6] = [
    ("rateLimitCooldownMs", 60_000.0),
    ("quotaCooldownMs", 900_000.0),
    ("authCooldownMs", 300_000.0),
    ("transientBaseCooldownMs", 1_000.0),
    ("maxCooldownMs", 3_600_000.0),
    ("errorsBeforeSwitch", 3.0),
];

const WEIGHT_DESCRIPTION: &str =
    "Traffic share under weighted round robin: 3 vs 1 gives roughly three of every four requests. 1 is an equal share.";
const PRIORITY_DESCRIPTION: &str =
    "Failover order under the priority strategy: smaller numbers run first (0 before 1), falling back only while a lower number cools down.";

/// `service.updateSchedulerDefaults(effective)`. `Port` covers the store,
/// `reconcile` and the runtime view; the service call is the one extra side
/// effect the pool manager callbacks perform.
pub trait PoolDefaults: Send + Sync {
    fn update_scheduler_defaults(&self, settings: Value);
}

/// `PoolManagerState` as the port reads it. `upstream` keeps the stored
/// preference shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PoolState {
    pub pool_exists: bool,
    pub policy: String,
    pub affinity: bool,
    pub include_upstream: bool,
    pub upstream: UpstreamPreferences,
    pub accounts: Vec<PoolAccount>,
    pub scheduler: Map<String, Value>,
    pub upstream_configured: bool,
    pub upstream_source: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpstreamPreferences {
    pub label: Option<String>,
    pub weight: Option<f64>,
    pub priority: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PoolAccount {
    pub id: String,
    pub label: String,
    pub auth_kind: String,
    pub enabled: bool,
    pub weight: f64,
    pub priority: f64,
}

impl PoolState {
    fn scheduler_value(&self, key: &str) -> Option<f64> {
        self.scheduler.get(key).and_then(Value::as_f64)
    }
}

fn default_for(key: &str) -> f64 {
    SCHEDULER_DEFAULTS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
        .unwrap_or(0.0)
}

fn account_from(value: &Value) -> PoolAccount {
    PoolAccount {
        id: value["id"].as_str().unwrap_or("").to_owned(),
        label: value["label"].as_str().unwrap_or("").to_owned(),
        auth_kind: value["authKind"].as_str().unwrap_or("").to_owned(),
        enabled: value["enabled"].as_bool().unwrap_or(false),
        weight: value["weight"].as_f64().unwrap_or(0.0),
        priority: value["priority"].as_f64().unwrap_or(0.0),
    }
}

fn upstream_from(value: &Value) -> UpstreamPreferences {
    UpstreamPreferences {
        label: value["label"].as_str().map(str::to_owned),
        weight: value["weight"].as_f64(),
        priority: value["priority"].as_f64(),
    }
}

/// `mergeUpstream`: an empty label and `Pi default` clear the stored label, and
/// non-integer or out-of-range numbers clear that preference.
fn merge_upstream(current: &UpstreamPreferences, field: &str, value: &str) -> UpstreamPreferences {
    let mut next = current.clone();
    match field {
        "label" => {
            next.label = None;
            let label = trim(value);
            if !label.is_empty() && label != UPSTREAM_DEFAULT_LABEL {
                next.label = Some(label.to_owned());
            }
        }
        "weight" => {
            next.weight = None;
            let parsed = settings::js_number_parse(value);
            if parsed.fract() == 0.0 && parsed.is_finite() && parsed >= 1.0 {
                next.weight = Some(parsed);
            }
        }
        _ => {
            next.priority = None;
            let parsed = settings::js_number_parse(value);
            if parsed.fract() == 0.0 && parsed.is_finite() && parsed >= 0.0 {
                next.priority = Some(parsed);
            }
        }
    }
    next
}

fn upstream_label(state: &PoolState) -> String {
    match state.upstream.label.as_deref() {
        Some(label) if !trim(label).is_empty() => label.to_owned(),
        _ => UPSTREAM_DEFAULT_LABEL.to_owned(),
    }
}

fn upstream_summary(state: &PoolState) -> String {
    if !state.include_upstream {
        return "disabled".to_owned();
    }
    format!(
        "weight {} · priority {}",
        settings::js_number(state.upstream.weight.unwrap_or(1.0)),
        settings::js_number(state.upstream.priority.unwrap_or(0.0))
    )
}

fn account_summary(account: &PoolAccount) -> String {
    if !account.enabled {
        return "disabled".to_owned();
    }
    format!(
        "weight {} · priority {}",
        settings::js_number(account.weight),
        settings::js_number(account.priority)
    )
}

fn scheduler_summary(state: &PoolState) -> String {
    let rate_limit = state
        .scheduler_value("rateLimitCooldownMs")
        .unwrap_or_else(|| default_for("rateLimitCooldownMs"));
    let quota = state
        .scheduler_value("quotaCooldownMs")
        .unwrap_or_else(|| default_for("quotaCooldownMs"));
    let auth = state
        .scheduler_value("authCooldownMs")
        .unwrap_or_else(|| default_for("authCooldownMs"));
    let errors = state
        .scheduler_value("errorsBeforeSwitch")
        .unwrap_or_else(|| default_for("errorsBeforeSwitch"));
    format!(
        "{} rate-limit · {} quota · {} auth · {} errors/switch",
        settings::format_ms(rate_limit),
        settings::format_ms(quota),
        settings::format_ms(auth),
        settings::js_number(errors)
    )
}

fn accounts_summary(state: &PoolState) -> String {
    let mut parts: Vec<String> = Vec::new();
    if state.pool_exists {
        parts.push(if state.include_upstream {
            "1 upstream".to_owned()
        } else {
            "1 upstream (disabled)".to_owned()
        });
    } else if state.upstream_configured {
        parts.push("1 upstream (pending)".to_owned());
    }
    let stored = state.accounts.len();
    if stored > 0 {
        let disabled = state.accounts.iter().filter(|account| !account.enabled).count();
        parts.push(if disabled == 0 {
            format!("{stored} stored")
        } else {
            format!("{stored} stored, {disabled} disabled")
        });
    }
    if parts.is_empty() {
        "empty".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// `selectAfterFor(id)` from the upstream component.
fn select_after_for(id: &str) -> &str {
    let head = id.split('.').next().unwrap_or(id);
    match head {
        "account" | "upstream" | "stored" => "accounts",
        _ => head,
    }
}

/// `PreferredAuthMethod` for the reauthenticate row.
fn preferred_auth_method(account: &PoolAccount, methods: &[PoolMethod]) -> String {
    let preferred = match account.auth_kind.as_str() {
        "oauth" => Some("oauth"),
        "api-key" => Some("api_key"),
        _ => None,
    };
    if let Some(preferred) = preferred {
        if methods.iter().any(|method| method.value == preferred) {
            return preferred.to_owned();
        }
    }
    if account.auth_kind == "api-key" {
        if let Some(method) = methods.iter().find(|method| method.value == "api_key_paste") {
            return method.value.clone();
        }
    }
    methods
        .first()
        .map(|method| method.value.clone())
        .unwrap_or_default()
}

// The screen tree ------------------------------------------------------------

enum SelectDone {
    Value,
    Add,
    Reauth,
    Remove,
}

enum Page {
    Settings {
        title: String,
        description: String,
        items: Vec<Row>,
    },
    Select {
        title: String,
        description: Option<String>,
        options: Vec<(String, String)>,
        selected: usize,
        done: SelectDone,
    },
    Input {
        title: String,
        description: String,
        initial: String,
        integer_min: Option<u64>,
    },
}

enum Row {
    Plain(SettingItem),
    Cycle(SettingItem),
    Section(SettingItem, Box<Page>),
    Leaf(SettingItem, Box<Page>),
}

impl Row {
    fn item(&self) -> &SettingItem {
        match self {
            Row::Plain(item) | Row::Cycle(item) | Row::Section(item, _) | Row::Leaf(item, _) => item,
        }
    }
}

fn select_page(
    title: impl Into<String>,
    description: Option<&str>,
    options: Vec<(String, String)>,
    current: Option<&str>,
    done: SelectDone,
) -> Page {
    let selected = current
        .and_then(|current| options.iter().position(|(value, _)| value == current))
        .unwrap_or(0);
    Page::Select {
        title: title.into(),
        description: description.map(str::to_owned),
        options,
        selected,
        done,
    }
}

fn settings_page(title: impl Into<String>, description: impl Into<String>, items: Vec<Row>) -> Page {
    let mut page = Page::Settings {
        title: title.into(),
        description: description.into(),
        items,
    };
    mark_drill_in_page(&mut page);
    page
}

fn upstream_row(state: &PoolState) -> Row {
    let label = upstream_label(state);
    let current = if state.pool_exists {
        upstream_summary(state)
    } else {
        format!(
            "{} · pending pool ✓",
            state.upstream_source.as_deref().unwrap_or("configured")
        )
    };
    let item = SettingItem::new("upstream", format!("{label} (upstream)"), current)
        .described("Pi's own /login, auth.json, environment, or ambient credential. Resolves live on every request and pools alongside stored accounts.")
        .drill_in();
    let items = vec![
        Row::Cycle(
            SettingItem::new("upstream.enabled", "Enabled", if state.include_upstream { "true" } else { "false" })
                .described("While enabled, this credential pools with stored accounts. Disabled, this pool runs multilogin accounts only.")
                .cycling(&BOOLEANS),
        ),
        Row::Leaf(
            SettingItem::new("upstream.label", "Label", label.clone())
                .described(format!("Shown in /accounts and selectors. Empty restores \"{UPSTREAM_DEFAULT_LABEL}\"."))
                .drill_in(),
            Box::new(Page::Input {
                title: "Account label".to_owned(),
                description: format!("Empty restores \"{UPSTREAM_DEFAULT_LABEL}\"."),
                initial: label.clone(),
                integer_min: None,
            }),
        ),
        Row::Leaf(
            SettingItem::new("upstream.weight", "Weight", settings::js_number(state.upstream.weight.unwrap_or(1.0)))
                .described(WEIGHT_DESCRIPTION)
                .drill_in(),
            Box::new(Page::Input {
                title: "Weight".to_owned(),
                description: WEIGHT_DESCRIPTION.to_owned(),
                initial: settings::js_number(state.upstream.weight.unwrap_or(1.0)),
                integer_min: Some(1),
            }),
        ),
        Row::Leaf(
            SettingItem::new("upstream.priority", "Priority", settings::js_number(state.upstream.priority.unwrap_or(0.0)))
                .described(PRIORITY_DESCRIPTION)
                .drill_in(),
            Box::new(Page::Input {
                title: "Priority".to_owned(),
                description: PRIORITY_DESCRIPTION.to_owned(),
                initial: settings::js_number(state.upstream.priority.unwrap_or(0.0)),
                integer_min: Some(0),
            }),
        ),
    ];
    Row::Section(
        item,
        Box::new(settings_page(
            label,
            "Pi's own credential resolves live on every request. It cannot be removed—disable it to exclude it from the pool.",
            items,
        )),
    )
}

fn stored_account_row(account: &PoolAccount, methods: &[PoolMethod]) -> Row {
    let item = SettingItem::new(
        format!("stored.{}", account.id),
        account.label.clone(),
        account_summary(account),
    )
    .described(format!(
        "{} credential stored in multiprovider-auth.json.",
        account.auth_kind
    ))
    .drill_in();
    let mut rows = vec![
        Row::Leaf(
            SettingItem::new(format!("account.{}.label", account.id), "Label", account.label.clone())
                .described("Non-secret display label for this credential.")
                .drill_in(),
            Box::new(Page::Input {
                title: "Account label".to_owned(),
                description: "Non-secret display label for this credential.".to_owned(),
                initial: account.label.clone(),
                integer_min: None,
            }),
        ),
        Row::Cycle(
            SettingItem::new(
                format!("account.{}.enabled", account.id),
                "Enabled",
                if account.enabled { "true" } else { "false" },
            )
            .described("Disabled accounts are skipped by selection and failover.")
            .cycling(&BOOLEANS),
        ),
        Row::Leaf(
            SettingItem::new(
                format!("account.{}.weight", account.id),
                "Weight",
                settings::js_number(account.weight),
            )
            .described(WEIGHT_DESCRIPTION)
            .drill_in(),
            Box::new(Page::Input {
                title: "Weight".to_owned(),
                description: WEIGHT_DESCRIPTION.to_owned(),
                initial: settings::js_number(account.weight),
                integer_min: Some(1),
            }),
        ),
        Row::Leaf(
            SettingItem::new(
                format!("account.{}.priority", account.id),
                "Priority",
                settings::js_number(account.priority),
            )
            .described(PRIORITY_DESCRIPTION)
            .drill_in(),
            Box::new(Page::Input {
                title: "Priority".to_owned(),
                description: PRIORITY_DESCRIPTION.to_owned(),
                initial: settings::js_number(account.priority),
                integer_min: Some(0),
            }),
        ),
    ];
    if !methods.is_empty() {
        let preferred = preferred_auth_method(account, methods);
        rows.push(Row::Leaf(
            SettingItem::new(
                format!("account.{}.reauth", account.id),
                "Reauthenticate",
                account.auth_kind.clone(),
            )
            .described("Run the provider login flow again and replace this stored credential in place. Label, weight, priority, and session pins stay; the account cooldown is cleared.")
            .drill_in(),
            Box::new(select_page(
                format!("Reauthenticate {}", account.label),
                Some("Choose the login method. The stored credential is replaced only after a successful login."),
                methods
                    .iter()
                    .map(|method| (method.value.clone(), method.label.clone()))
                    .collect(),
                Some(&preferred),
                SelectDone::Reauth,
            )),
        ));
    }
    rows.push(Row::Leaf(
        SettingItem::new(format!("account.{}.remove", account.id), "Remove account", "")
            .described("Permanently deletes this stored credential.")
            .drill_in(),
        Box::new(select_page(
            format!("Remove {}?", account.label),
            Some("The stored credential is deleted; Pi's normal /login credential is unchanged."),
            vec![
                ("keep".to_owned(), "Cancel".to_owned()),
                ("remove".to_owned(), format!("Remove {}", account.label)),
            ],
            Some("keep"),
            SelectDone::Remove,
        )),
    ));
    Row::Section(
        item,
        Box::new(settings_page(
            account.label.clone(),
            "Stored multilogin account. Values apply to the next selection.",
            rows,
        )),
    )
}

fn accounts_row(provider: &ProviderInfo, state: &PoolState, methods: &[PoolMethod]) -> Row {
    let item = SettingItem::new("accounts", "Accounts", accounts_summary(state))
        .described("Credentials pooled for this provider: Pi's upstream /login credential plus stored multilogin accounts.")
        .drill_in();
    let mut rows: Vec<Row> = Vec::new();
    if state.pool_exists || state.upstream_configured {
        rows.push(upstream_row(state));
    }
    for account in &state.accounts {
        rows.push(stored_account_row(account, methods));
    }
    if rows.is_empty() {
        rows.push(Row::Plain(
            SettingItem::new("accounts.none", "No accounts yet", "")
                .described("Use the Add account row below to store a credential for this pool."),
        ));
    }
    if !methods.is_empty() {
        rows.push(Row::Leaf(
            SettingItem::new(
                "accounts.add",
                "Add account",
                methods
                    .iter()
                    .map(|method| method.label.clone())
                    .collect::<Vec<_>>()
                    .join(" · "),
            )
            .described("Run the provider login flow and store a new pooled credential.")
            .drill_in(),
            Box::new(select_page(
                format!("Add {} account", provider.name),
                Some("Choose the login method for the new account."),
                methods
                    .iter()
                    .map(|method| (method.value.clone(), method.label.clone()))
                    .collect(),
                methods.first().map(|method| method.value.as_str()),
                SelectDone::Add,
            )),
        ));
    }
    Row::Section(
        item,
        Box::new(settings_page(
            format!("{} accounts", provider.name),
            "Each credential participating in this pool. Values apply to the next selection.",
            rows,
        )),
    )
}

fn cooldown_row(
    state: &PoolState,
    key: &str,
    label: &str,
    description: &str,
    options: &[f64],
) -> Row {
    let fallback = default_for(key);
    let configured = state.scheduler_value(key);
    let current = match configured {
        None => format!("{} · default", settings::format_ms(fallback)),
        Some(value) => settings::format_ms(value),
    };
    let selected_value = match configured {
        None => "default".to_owned(),
        Some(value) => settings::js_number(value),
    };
    let mut items: Vec<(String, String)> = vec![(
        "default".to_owned(),
        format!("Default ({})", settings::format_ms(fallback)),
    )];
    for option in options {
        items.push((
            settings::js_number(*option),
            settings::format_ms(*option),
        ));
    }
    if !items.iter().any(|(value, _)| *value == selected_value) {
        items.push((
            selected_value.clone(),
            settings::format_ms(configured.unwrap_or(fallback)),
        ));
    }
    Row::Leaf(
        SettingItem::new(format!("scheduler.{key}"), label, current)
            .described(description)
            .drill_in(),
        Box::new(select_page(label, Some(description), items, Some(&selected_value), SelectDone::Value)),
    )
}

fn count_row(state: &PoolState, key: &str, label: &str, description: &str, options: &[f64]) -> Row {
    let fallback = default_for(key);
    let configured = state.scheduler_value(key);
    let format_count = |value: f64| settings::js_number(value);
    let current = match configured {
        None => format!("{} · default", format_count(fallback)),
        Some(value) => format_count(value),
    };
    let selected_value = match configured {
        None => "default".to_owned(),
        Some(value) => format_count(value),
    };
    let mut items: Vec<(String, String)> = vec![(
        "default".to_owned(),
        format!("Default ({})", format_count(fallback)),
    )];
    for option in options {
        items.push((format_count(*option), format_count(*option)));
    }
    if !items.iter().any(|(value, _)| *value == selected_value) {
        items.push((
            selected_value.clone(),
            format_count(configured.unwrap_or(fallback)),
        ));
    }
    Row::Leaf(
        SettingItem::new(format!("scheduler.{key}"), label, current)
            .described(description)
            .drill_in(),
        Box::new(select_page(label, Some(description), items, Some(&selected_value), SelectDone::Value)),
    )
}

fn scheduler_row(state: &PoolState) -> Row {
    let item = SettingItem::new("scheduler", "Scheduler", scheduler_summary(state))
        .described("Global failure cooldowns applied to every pool. Saves immediately.")
        .drill_in();
    let mut rows = vec![
        cooldown_row(state, "rateLimitCooldownMs", "Rate-limit cooldown", "Cooldown after HTTP 429 / rate-limit failures. Default 60s.", &RATE_LIMIT_OPTIONS),
        cooldown_row(state, "quotaCooldownMs", "Quota cooldown", "Cooldown after quota or credit exhaustion (HTTP 402). Default 15m.", &QUOTA_OPTIONS),
        cooldown_row(state, "authCooldownMs", "Auth cooldown", "Cooldown after 401/403 or invalid/expired credentials. Default 5m.", &AUTH_OPTIONS),
        cooldown_row(state, "transientBaseCooldownMs", "Transient base cooldown", "Base for exponential backoff on transient (5xx/408/425) failures; doubles per consecutive failure. Default 1s.", &TRANSIENT_OPTIONS),
        cooldown_row(state, "maxCooldownMs", "Max cooldown", "Upper bound applied to every cooldown. Default 60m.", &MAX_COOLDOWN_OPTIONS),
        count_row(state, "errorsBeforeSwitch", "Errors before switch", "Consecutive pre-output errors absorbed on the same account before failing over to the next account or provider. Default 3.", &COUNT_OPTIONS),
    ];
    Row::Section(
        item,
        Box::new(settings_page(
            "Scheduler cooldowns",
            "Global failure cooldowns applied to every pool. Changes save and apply immediately.",
            std::mem::take(&mut rows),
        )),
    )
}

fn mark_drill_in_page(page: &mut Page) {
    if let Page::Settings { items, .. } = page {
        let mut plain: Vec<SettingItem> = items.iter().map(|row| row.item().clone()).collect();
        settings::mark_drill_in(&mut plain);
        for (row, marked) in items.iter_mut().zip(plain.into_iter()) {
            match row {
                Row::Plain(item) | Row::Cycle(item) | Row::Section(item, _) | Row::Leaf(item, _) => {
                    *item = marked;
                }
            }
        }
        for row in items.iter_mut() {
            if let Row::Section(_, child) = row {
                mark_drill_in_page(child);
            }
        }
    }
}

fn build_view(provider: &ProviderInfo, state: &PoolState, methods: &[PoolMethod]) -> Page {
    settings_page(
        format!("{} pool", provider.name),
        if state.pool_exists {
            format!(
                "{} stored {} · changes save immediately",
                state.accounts.len(),
                if state.accounts.len() == 1 { "account" } else { "accounts" }
            )
        } else if state.upstream_configured {
            "No pool yet · upstream credential detected · settings apply once the first account is added".to_owned()
        } else {
            "No pool yet · settings apply once the first account is added".to_owned()
        },
        vec![
            Row::Cycle(
                SettingItem::new("pool.policy", "Strategy", state.policy.clone())
                    .described("How the pool picks the next account for each request.")
                    .cycling_owned(POLICIES.iter().map(|policy| (*policy).to_owned()).collect()),
            ),
            Row::Cycle(
                SettingItem::new(
                    "pool.affinity",
                    "Session affinity",
                    if state.affinity { "true" } else { "false" },
                )
                .described("Keep a healthy account pinned to this session instead of re-selecting on every request.")
                .cycling(&BOOLEANS),
            ),
            accounts_row(provider, state, methods),
            scheduler_row(state),
        ],
    )
}

// Driving the screen ---------------------------------------------------------

enum Dispatch {
    Changed,
    Exit(PoolAction),
}

enum Outcome {
    Cancelled,
    Exit(PoolAction),
    Refresh(String),
}

struct Frame<'a> {
    page: &'a Page,
    query: String,
    selected: String,
    row_id: Option<String>,
}

fn item_values(items: &[Row]) -> Vec<SettingItem> {
    items.iter().map(|row| row.item().clone()).collect()
}

fn first_item_id(page: &Page) -> String {
    match page {
        Page::Settings { items, .. } => items
            .first()
            .map(|row| row.item().id.clone())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn selection_after(items: &[Row], id: &str) -> String {
    let after = select_after_for(id);
    if items.iter().any(|row| row.item().id == after) {
        after.to_owned()
    } else {
        items
            .first()
            .map(|row| row.item().id.clone())
            .unwrap_or_default()
    }
}

fn find_row<'a>(items: &'a [Row], id: &str) -> Option<&'a Row> {
    items.iter().find(|row| row.item().id == id)
}

async fn load_state<P: Port>(
    port: &P,
    provider: &ProviderInfo,
    buffer: &mut PoolBuffer,
) -> Result<PoolState, String> {
    let pool = port
        .store(json!({"op": "pool", "provider": provider.id}))
        .await?;
    let scheduler = port.store(json!({"op": "getScheduler"})).await?;
    let status = port.runtime().map(|runtime| runtime.status(&provider.id));
    let upstream_configured = status.as_ref().map(|status| status.configured).unwrap_or(false);
    let upstream_source = if upstream_configured {
        status
            .as_ref()
            .and_then(|status| status.label.clone().or_else(|| status.source.clone()))
    } else {
        None
    };
    let mut state = PoolState {
        upstream_configured,
        upstream_source,
        scheduler: scheduler.as_object().cloned().unwrap_or_default(),
        ..PoolState::default()
    };
    if pool.is_null() {
        state.pool_exists = false;
        state.policy = buffer.policy.clone();
        state.affinity = buffer.affinity;
        state.include_upstream = buffer.include_upstream;
        state.upstream = upstream_from(&buffer.upstream);
        return Ok(state);
    }
    let policy = pool["policy"].as_str().unwrap_or("round-robin").to_owned();
    let affinity = pool["affinity"].as_bool().unwrap_or(true);
    let include_upstream = pool["includeUpstream"].as_bool().unwrap_or(true);
    buffer.policy = policy.clone();
    buffer.affinity = affinity;
    buffer.include_upstream = include_upstream;
    buffer.upstream = pool["upstream"].clone();
    if buffer.upstream.is_null() {
        buffer.upstream = json!({});
    }
    state.pool_exists = true;
    state.policy = policy;
    state.affinity = affinity;
    state.include_upstream = include_upstream;
    state.upstream = upstream_from(&buffer.upstream);
    state.accounts = pool["accounts"]
        .as_array()
        .map(|accounts| accounts.iter().map(account_from).collect())
        .unwrap_or_default();
    Ok(state)
}

async fn update_pool<P: Port>(
    port: &P,
    provider: &ProviderInfo,
    buffer: &mut PoolBuffer,
    patch: Value,
) -> Result<(), String> {
    let pool = port
        .store(json!({"op": "pool", "provider": provider.id}))
        .await?;
    if pool.is_null() {
        if let Some(policy) = patch.get("policy").and_then(Value::as_str) {
            buffer.policy = policy.to_owned();
        }
        if let Some(affinity) = patch.get("affinity").and_then(Value::as_bool) {
            buffer.affinity = affinity;
        }
        if let Some(include) = patch.get("includeUpstream").and_then(Value::as_bool) {
            buffer.include_upstream = include;
        }
        if let Some(upstream) = patch.get("upstream").filter(|value| !value.is_null()) {
            buffer.upstream = upstream.clone();
        }
        return Ok(());
    }
    port.store(json!({"op": "setPool", "provider": provider.id, "input": patch}))
        .await?;
    port.reconcile().await
}

async fn update_account<P: Port>(
    port: &P,
    provider: &ProviderInfo,
    account_id: &str,
    patch: Value,
) -> Result<(), String> {
    port.store(json!({
        "op": "account",
        "provider": provider.id,
        "account": account_id,
        "input": patch,
    }))
    .await?;
    port.reconcile().await
}

async fn remove_account<P: Port>(
    port: &P,
    provider: &ProviderInfo,
    account_id: &str,
) -> Result<(), String> {
    port.store(json!({
        "op": "remove",
        "provider": provider.id,
        "account": account_id,
    }))
    .await?;
    port.reconcile().await
}

async fn update_scheduler<P: Port + PoolDefaults>(
    port: &P,
    key: &str,
    value_ms: Option<f64>,
) -> Result<(), String> {
    let mut request = json!({"op": "setScheduler", "input": {}});
    match value_ms {
        Some(value) if value.is_finite() && value >= 0.0 => {
            request["input"][key] = json!(value);
        }
        Some(_) => {
            return Err(format!(
                "multiprovider: scheduler setting \"{key}\" must be a non-negative number"
            ));
        }
        None => {
            request["clear"] = json!([key]);
        }
    }
    let effective = port.store(request).await?;
    port.update_scheduler_defaults(effective);
    Ok(())
}

async fn dispatch<U: Ui, P: Port + PoolDefaults>(
    ui: &U,
    port: &P,
    provider: &ProviderInfo,
    state: &PoolState,
    buffer: &mut PoolBuffer,
    id: &str,
    value: &str,
) -> Result<Dispatch, String> {
    if id == "accounts.add" {
        if let Some(method) = value.strip_prefix(ADD_SENTINEL_PREFIX) {
            return Ok(Dispatch::Exit(PoolAction::Add {
                method: method.to_owned(),
            }));
        }
        return Ok(Dispatch::Changed);
    }
    if id == "pool.policy" {
        update_pool(port, provider, buffer, json!({"policy": value})).await?;
        return Ok(Dispatch::Changed);
    }
    if id == "pool.affinity" {
        update_pool(port, provider, buffer, json!({"affinity": value == "true"})).await?;
        return Ok(Dispatch::Changed);
    }
    if id == "upstream.enabled" {
        update_pool(port, provider, buffer, json!({"includeUpstream": value == "true"})).await?;
        return Ok(Dispatch::Changed);
    }
    if let Some(field) = id.strip_prefix("upstream.") {
        if matches!(field, "label" | "weight" | "priority") {
            let merged = merge_upstream(&state.upstream, field, value);
            let upstream = json!({
                "label": merged.label,
                "weight": merged.weight,
                "priority": merged.priority,
            });
            let mut cleaned = upstream.as_object().cloned().unwrap_or_default();
            cleaned.retain(|_, value| !value.is_null());
            update_pool(port, provider, buffer, json!({"upstream": Value::Object(cleaned)})).await?;
            return Ok(Dispatch::Changed);
        }
    }
    if let Some(key) = id.strip_prefix("scheduler.") {
        if !SCHEDULER_SETTING_KEYS.contains(&key) {
            return Ok(Dispatch::Changed);
        }
        let value_ms = if value == "default" {
            None
        } else {
            Some(settings::js_number_parse(value))
        };
        update_scheduler(port, key, value_ms).await?;
        return Ok(Dispatch::Changed);
    }
    if let Some(rest) = id.strip_prefix("account.") {
        let Some((account_id, field)) = rest.rsplit_once('.') else {
            return Ok(Dispatch::Changed);
        };
        match field {
            "remove" => {
                if value == REMOVE_SENTINEL {
                    remove_account(port, provider, account_id).await?;
                }
            }
            "reauth" => {
                if let Some(method) = value.strip_prefix(REAUTH_SENTINEL_PREFIX) {
                    return Ok(Dispatch::Exit(PoolAction::Reauth {
                        account_id: account_id.to_owned(),
                        method: method.to_owned(),
                    }));
                }
            }
            "label" => {
                let label = trim(value);
                if label.is_empty() {
                    notify(ui, "Account label must not be empty.", "error").await?;
                } else {
                    update_account(port, provider, account_id, json!({"label": label})).await?;
                }
            }
            "enabled" => {
                update_account(port, provider, account_id, json!({"enabled": value == "true"})).await?;
            }
            "weight" | "priority" => {
                let parsed = settings::js_number_parse(value);
                let min = if field == "weight" { 1.0 } else { 0.0 };
                if parsed.fract() == 0.0 && parsed.is_finite() && parsed >= min {
                    update_account(port, provider, account_id, json!({field: parsed})).await?;
                }
            }
            _ => {}
        }
    }
    Ok(Dispatch::Changed)
}

async fn drive<U: Ui, P: Port + PoolDefaults>(
    ui: &U,
    port: &P,
    provider: &ProviderInfo,
    state: &PoolState,
    page: &Page,
    initial_selected: Option<String>,
    buffer: &mut PoolBuffer,
) -> Result<Outcome, String> {
    let root_selected = initial_selected.unwrap_or_else(|| first_item_id(page));
    let mut frames: Vec<Frame> = vec![Frame {
        page,
        query: String::new(),
        selected: root_selected,
        row_id: None,
    }];
    loop {
        let index = frames.len() - 1;
        let current = frames[index].page;
        let reply = match current {
            Page::Settings {
                title,
                description,
                items,
            } => {
                let control = settings::settings_control(
                    title,
                    description,
                    &item_values(items),
                    &frames[index].query,
                    Some(&frames[index].selected),
                );
                ui.show(control).await?
            }
            Page::Select {
                title,
                description,
                options,
                selected,
                ..
            } => {
                let control = settings::select_control(title, description.as_deref(), options, *selected);
                ui.show(control).await?
            }
            Page::Input {
                title,
                description,
                initial,
                integer_min,
            } => {
                let control = settings::input_control(title, description, initial, *integer_min);
                ui.show(control).await?
            }
        };
        let Some(reply) = reply else {
            if index == 0 {
                return Ok(Outcome::Cancelled);
            }
            frames.pop();
            continue;
        };
        if let Some(query) = reply.get("query").and_then(Value::as_str) {
            frames[index].query = query.to_owned();
        }
        match current {
            Page::Settings { items, .. } => {
                if settings::settings_opened(&reply) {
                    let id = reply["id"]
                        .as_str()
                        .ok_or_else(|| "setting open requires an ID".to_owned())?;
                    let row = find_row(items, id).ok_or_else(|| format!("unknown setting row \"{id}\""))?;
                    frames[index].selected = id.to_owned();
                    let (child, row_id) = match row {
                        Row::Section(_, child) | Row::Leaf(_, child) => (child.as_ref(), id.to_owned()),
                        Row::Plain(_) | Row::Cycle(_) => continue,
                    };
                    frames.push(Frame {
                        page: child,
                        query: String::new(),
                        selected: first_item_id(child),
                        row_id: Some(row_id),
                    });
                    continue;
                }
                let (id, value) = settings::settings_reply(&reply)?
                    .ok_or_else(|| "setting change requires an ID".to_owned())?;
                match dispatch(ui, port, provider, state, buffer, &id, &value).await {
                    Ok(Dispatch::Exit(action)) => return Ok(Outcome::Exit(action)),
                    Ok(Dispatch::Changed) => return Ok(Outcome::Refresh(id)),
                    // Upstream's persist queue reports a failed change as an
                    // error notification and still reloads the screen.
                    Err(message) => {
                        notify(ui, &message, "error").await?;
                        return Ok(Outcome::Refresh(id));
                    }
                }
            }
            Page::Select { done, .. } => {
                let Some(value) = settings::select_reply(&reply)? else {
                    frames.pop();
                    continue;
                };
                let mapped = match done {
                    SelectDone::Value => Some(value),
                    SelectDone::Add => Some(format!("{ADD_SENTINEL_PREFIX}{value}")),
                    SelectDone::Reauth => Some(format!("{REAUTH_SENTINEL_PREFIX}{value}")),
                    SelectDone::Remove => (value == "remove").then(|| REMOVE_SENTINEL.to_owned()),
                };
                let Some(mapped) = mapped else {
                    frames.pop();
                    continue;
                };
                let id = frames[index]
                    .row_id
                    .clone()
                    .ok_or_else(|| "a select submenu needs an owning row".to_owned())?;
                match dispatch(ui, port, provider, state, buffer, &id, &mapped).await {
                    Ok(Dispatch::Exit(action)) => return Ok(Outcome::Exit(action)),
                    Ok(Dispatch::Changed) => return Ok(Outcome::Refresh(id)),
                    Err(message) => {
                        notify(ui, &message, "error").await?;
                        return Ok(Outcome::Refresh(id));
                    }
                }
            }
            Page::Input { integer_min, .. } => {
                let value = match integer_min {
                    Some(min) => match settings::integer_reply(&reply, *min) {
                        Ok(Some(number)) => settings::js_number(number),
                        Ok(None) => {
                            frames.pop();
                            continue;
                        }
                        Err(_) => continue,
                    },
                    None => match settings::input_reply(&reply)? {
                        Some(value) => value,
                        None => {
                            frames.pop();
                            continue;
                        }
                    },
                };
                let id = frames[index]
                    .row_id
                    .clone()
                    .ok_or_else(|| "an input submenu needs an owning row".to_owned())?;
                match dispatch(ui, port, provider, state, buffer, &id, &value).await {
                    Ok(Dispatch::Exit(action)) => return Ok(Outcome::Exit(action)),
                    Ok(Dispatch::Changed) => return Ok(Outcome::Refresh(id)),
                    Err(message) => {
                        notify(ui, &message, "error").await?;
                        return Ok(Outcome::Refresh(id));
                    }
                }
            }
        }
    }
}

/// `openPoolManager(ctx, provider, callbacks, methods)`: shows the pool manager
/// screen until the user closes it, adds an account or asks to reauthenticate.
/// The buffer follows upstream: while no pool is stored, edits accumulate in it.
pub async fn open<U: Ui, P: Port + PoolDefaults>(
    ui: &U,
    port: &P,
    provider: &ProviderInfo,
    methods: &[PoolMethod],
    buffer: &mut PoolBuffer,
) -> Result<PoolAction, String> {
    let mut state = load_state(port, provider, buffer).await?;
    let mut select_after: Option<String> = None;
    loop {
        let page = build_view(provider, &state, methods);
        let initial = select_after.take().map(|id| match &page {
            Page::Settings { items, .. } => selection_after(items, &id),
            _ => id,
        });
        match drive(ui, port, provider, &state, &page, initial, buffer).await? {
            Outcome::Cancelled => return Ok(PoolAction::Closed),
            Outcome::Exit(action) => return Ok(action),
            Outcome::Refresh(id) => {
                select_after = Some(id);
                state = load_state(port, provider, buffer).await?;
            }
        }
    }
}
