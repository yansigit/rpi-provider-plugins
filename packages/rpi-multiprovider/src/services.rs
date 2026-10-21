use crate::types::*;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

type Inventory = Pin<Box<dyn Future<Output = Result<Vec<Account>, String>> + Send>>;
pub type Classifier =
    dyn Fn(&AttemptFailure, &Account) -> Result<Option<Disposition>, String> + Send + Sync;

pub struct Registration {
    pub id: String,
    pub label: String,
    pub accounts: Arc<dyn Fn() -> Inventory + Send + Sync>,
    pub classify_failure: Option<Arc<Classifier>>,
    pub management_hint: Option<String>,
    pub selection_bias: SelectionBias,
}

pub struct Options {
    pub default_policy: SelectionPolicy,
    pub affinity: bool,
    pub settings: Settings,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub random_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub random_int: Arc<dyn Fn(f64) -> Result<u64, Error> + Send + Sync>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            default_policy: SelectionPolicy::default(),
            affinity: true,
            settings: Settings::default(),
            now: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as f64
            }),
            random_id: Arc::new(|| uuid::Uuid::new_v4().to_string()),
            random_int: Arc::new(random_int),
        }
    }
}

fn random_int(max: f64) -> Result<u64, Error> {
    // Node crypto.randomInt requires an integer range strictly below 2^48.
    if !max.is_finite() || max.fract() != 0.0 || max.abs() > 9_007_199_254_740_991.0 {
        let number = if !max.is_finite() {
            if max.is_nan() {
                "NaN".into()
            } else {
                "Infinity".into()
            }
        } else if max.fract() == 0.0 && max.abs() < 1e21 {
            format!("{max:.0}")
        } else {
            serde_json::to_string(&max).unwrap()
        };
        return Err(Error::Typed(
            "TypeError",
            format!("The \"max\" argument must be a safe integer. Received type number ({number})"),
        ));
    }
    if max <= 0.0 || max >= (1u64 << 48) as f64 {
        let number = format!("{max:.0}");
        let grouped: String = number
            .chars()
            .enumerate()
            .flat_map(|(i, digit)| {
                if i > 0 && !(i == 1 && number.starts_with('-')) && (number.len() - i) % 3 == 0 {
                    vec!['_', digit]
                } else {
                    vec![digit]
                }
            })
            .collect();
        let (argument, requirement) = if max <= 0.0 {
            ("max", "greater than the value of \"min\" (0)")
        } else {
            ("max - min", "<= 281474976710655")
        };
        return Err(Error::Typed("RangeError", format!("The value of \"{argument}\" is out of range. It must be {requirement}. Received {grouped}")));
    }
    let max = max as u64;
    let limit = u64::MAX - u64::MAX % max;
    loop {
        let mut bytes = [0; 8];
        getrandom::getrandom(&mut bytes)
            .map_err(|_| Error::Message("multiprovider: random source unavailable".into()))?;
        let value = u64::from_ne_bytes(bytes);
        if value < limit {
            return Ok(value % max);
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Error {
    UnknownProvider(String),
    UnknownAccount(String, String),
    NoAccountAvailable(String, Option<f64>),
    InvalidTime,
    Typed(&'static str, String),
    Message(String),
}
impl Error {
    pub fn name(&self) -> &'static str {
        match self {
            Self::UnknownProvider(_) => "UnknownProviderError",
            Self::UnknownAccount(_, _) => "UnknownAccountError",
            Self::NoAccountAvailable(_, _) => "NoAccountAvailableError",
            Self::InvalidTime => "RangeError",
            Self::Typed(name, _) => name,
            Self::Message(_) => "Error",
        }
    }
}
impl From<String> for Error {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider(id) => write!(f, "multiprovider: unknown provider \"{id}\""),
            Self::UnknownAccount(id, account) => write!(
                f,
                "multiprovider: provider \"{id}\" has no account \"{account}\""
            ),
            Self::NoAccountAvailable(id, None) => write!(
                f,
                "multiprovider: provider \"{id}\" has no enabled accounts"
            ),
            Self::NoAccountAvailable(id, Some(at)) => {
                let iso = iso_date(*at);
                write!(
                    f,
                    "multiprovider: provider \"{id}\" has no account available until {iso}"
                )
            }
            Self::InvalidTime => f.write_str("Invalid time value"),
            Self::Typed(_, message) => f.write_str(message),
            Self::Message(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {}

pub(crate) fn iso_date(at: f64) -> String {
    // Date.toISOString spans years beyond common platform time libraries.
    let millis = at as i64;
    let z = millis.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let day = z - era * 146_097;
    let year = (day - day / 1460 + day / 36_524 - day / 146_096) / 365;
    let day = day - (365 * year + year / 4 - year / 100);
    let month = (5 * day + 2) / 153;
    let day = day - (153 * month + 2) / 5 + 1;
    let month = month + if month < 10 { 3 } else { -9 };
    let year = year + era * 400 + i64::from(month <= 2);
    let year = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{year:+07}")
    };
    let time = millis.rem_euclid(86_400_000);
    format!(
        "{year}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3_600_000,
        time / 60_000 % 60,
        time / 1000 % 60,
        time % 1000
    )
}

#[derive(Default)]
struct Health {
    in_flight: u64,
    consecutive_failures: u64,
    cooldown_until: f64,
    last_selected_at: Option<f64>,
    last_failure_kind: Option<FailureKind>,
}
struct EffectiveAccount {
    account: Account,
    enabled: bool,
    weight: f64,
    priority: f64,
    runtime: Arc<Mutex<Health>>,
}
struct State {
    providers: IndexMap<String, (u64, Arc<Registration>)>,
    registration_sequence: u64,
    preferences: HashMap<String, PoolPreference>,
    runtime: HashMap<(String, String), Arc<Mutex<Health>>>,
    model_runtime: IndexMap<(String, String, String), Health>,
    affinity: HashMap<String, HashMap<String, AffinityPin>>,
    cursor: HashMap<String, u64>,
    scores: HashMap<String, HashMap<String, f64>>,
    options: Options,
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|error| error.into_inner())
}

#[derive(Clone)]
pub struct Service(Arc<Mutex<State>>);
impl Default for Service {
    fn default() -> Self {
        Self::new(Options::default())
    }
}
impl Service {
    pub fn new(options: Options) -> Self {
        Self(Arc::new(Mutex::new(State {
            providers: IndexMap::new(),
            registration_sequence: 0,
            preferences: HashMap::new(),
            runtime: HashMap::new(),
            model_runtime: IndexMap::new(),
            affinity: HashMap::new(),
            cursor: HashMap::new(),
            scores: HashMap::new(),
            options,
        })))
    }
    pub fn register(&self, registration: Registration) -> Result<Arc<Registration>, Error> {
        let mut state = lock(&self.0);
        if trim(&registration.id).is_empty() {
            return Err(Error::Message(
                "multiprovider: provider id must not be empty".into(),
            ));
        }
        if state.providers.contains_key(&registration.id) {
            return Err(Error::Message(format!(
                "multiprovider: duplicate provider \"{}\"",
                registration.id
            )));
        }
        let registration = Arc::new(registration);
        state.registration_sequence =
            state.registration_sequence.checked_add(1).ok_or_else(|| {
                Error::Message("multiprovider: registration sequence exhausted".into())
            })?;
        let sequence = state.registration_sequence;
        state
            .providers
            .insert(registration.id.clone(), (sequence, registration.clone()));
        Ok(registration)
    }
    pub fn unregister(&self, registration: &Arc<Registration>) {
        let mut state = lock(&self.0);
        let id = &registration.id;
        if !state
            .providers
            .get(id)
            .is_some_and(|(_, current)| Arc::ptr_eq(current, registration))
        {
            return;
        }
        state.providers.shift_remove(id);
        state.affinity.remove(id);
        state.cursor.remove(id);
        state.scores.remove(id);
        state
            .model_runtime
            .retain(|(provider, _, _), _| provider != id);
        // The pinned implementation retains preferences and account-wide health across re-registration.
    }
    pub fn has_provider(&self, id: &str) -> bool {
        lock(&self.0).providers.contains_key(id)
    }

    fn capture(&self, id: &str) -> Result<(Arc<Registration>, PoolPreference), Error> {
        let state = lock(&self.0);
        Ok((state.registration(id)?, state.pool(id)))
    }
    async fn effective(
        &self,
        registration: &Registration,
        pool: &PoolPreference,
    ) -> Result<Vec<EffectiveAccount>, Error> {
        let inventory = (registration.accounts)().await?;
        let mut state = lock(&self.0);
        let live: HashSet<_> = inventory
            .iter()
            .map(|account| account.id.as_str())
            .collect();
        state.model_runtime.retain(|(provider, _, account), _| {
            provider != &registration.id || live.contains(account.as_str())
        });
        let mut seen = HashSet::new();
        inventory
            .into_iter()
            .map(|account| {
                if trim(&account.id).is_empty() {
                    return Err(Error::Message(format!(
                        "multiprovider: provider \"{}\" returned an empty account id",
                        registration.id
                    )));
                }
                if !seen.insert(account.id.clone()) {
                    return Err(Error::Message(format!(
                        "multiprovider: provider \"{}\" returned duplicate account \"{}\"",
                        registration.id, account.id
                    )));
                }
                let preference = pool
                    .accounts
                    .iter()
                    .rev()
                    .find(|preference| preference.account_id == account.id);
                Ok(EffectiveAccount {
                    enabled: account.enabled != Some(false)
                        && preference.map_or(true, |p| p.enabled),
                    weight: preference
                        .map(|p| p.weight)
                        .or(account.weight)
                        .unwrap_or(1.0),
                    priority: preference
                        .map(|p| p.priority)
                        .or(account.priority)
                        .unwrap_or(0.0),
                    runtime: state.runtime_for(&registration.id, &account.id),
                    account,
                })
            })
            .collect()
    }
    pub async fn has_enabled_accounts(&self, id: &str) -> Result<bool, Error> {
        let (registration, pool) = self.capture(id)?;
        Ok(self
            .effective(&registration, &pool)
            .await?
            .iter()
            .any(|account| account.enabled))
    }
    pub async fn acquire(&self, options: AcquireOptions) -> Result<Lease, Error> {
        let (registration, pool) = self.capture(&options.provider_id)?;
        let accounts = self.effective(&registration, &pool).await?;
        let mut state = lock(&self.0);
        let now = (state.options.now)();
        let mut future = Vec::new();
        let available: Vec<_> = accounts
            .iter()
            .filter(|account| {
                if !account.enabled || options.exclude_account_ids.contains(&account.account.id) {
                    return false;
                }
                let blocking = state.blocking(&options, account, now);
                let ready = blocking.is_empty();
                future.extend(blocking);
                ready
            })
            .collect();
        if available.is_empty() {
            let future = future.into_iter().reduce(f64::min);
            if future.is_some_and(|at| !at.is_finite() || at.abs() > 8_640_000_000_000_000.0) {
                return Err(Error::InvalidTime);
            }
            return Err(Error::NoAccountAvailable(options.provider_id, future));
        }
        let pin = options
            .affinity_key
            .as_ref()
            .and_then(|key| state.affinity.get(&options.provider_id)?.get(key).cloned());
        let mut explicit = pin.as_ref().is_some_and(|pin| pin.explicit);
        let mut pinned = pin.map(|pin| pin.account_id);
        if explicit
            && !accounts
                .iter()
                .any(|account| account.enabled && Some(&account.account.id) == pinned.as_ref())
        {
            if let Some(pins) = state.affinity.get_mut(&options.provider_id) {
                pins.remove(options.affinity_key.as_deref().unwrap());
            }
            pinned = None;
            explicit = false;
        }
        let pinned_index = if pool.affinity || explicit {
            available
                .iter()
                .position(|account| Some(&account.account.id) == pinned.as_ref())
        } else {
            None
        };
        let selected = available[match pinned_index {
            Some(index) => index,
            None => state.select(&registration.id, pool.policy, &available)?,
        }];
        if pool.affinity && !explicit {
            if let Some(key) = &options.affinity_key {
                state
                    .affinity
                    .entry(options.provider_id.clone())
                    .or_default()
                    .insert(
                        key.clone(),
                        AffinityPin {
                            account_id: selected.account.id.clone(),
                            explicit: false,
                        },
                    );
            }
        }
        let id = (state.options.random_id)();
        let mut runtime = lock(&selected.runtime);
        runtime.in_flight += 1;
        runtime.last_selected_at = Some(now);
        drop(runtime);
        Ok(Lease {
            id,
            provider_id: options.provider_id,
            account: selected.account.clone(),
            acquired_at: now,
            model_id: options.model_id,
            runtime: Some(selected.runtime.clone()),
            registration,
            service: self.clone(),
        })
    }
    pub async fn snapshot(&self) -> Result<Value, Error> {
        let mut providers = Vec::new();
        let mut visited = 0;
        loop {
            // Pi's Map iterator observes registrations added while an inventory is awaited.
            let next = lock(&self.0)
                .providers
                .values()
                .find(|(sequence, _)| *sequence > visited)
                .cloned();
            let Some((sequence, registration)) = next else {
                break;
            };
            visited = sequence;
            let pool = lock(&self.0).pool(&registration.id);
            let accounts = self.effective(&registration, &pool).await?;
            let state = lock(&self.0);
            let now = (state.options.now)();
            let accounts: Vec<_> = accounts.into_iter().map(|account| {
                let runtime = lock(&account.runtime);
                let mut value = json!({"id":account.account.id,"label":account.account.label,"authKind":account.account.auth_kind,
                    "enabled":account.enabled,"weight":account.weight,"priority":account.priority,
                    "status":if !account.enabled { "disabled" } else if runtime.cooldown_until > now { "cooldown" } else { "ready" },
                    "inFlight":runtime.in_flight,"consecutiveFailures":runtime.consecutive_failures,
                    "metadata":account.account.metadata.unwrap_or_else(|| json!({}))});
                if runtime.cooldown_until > now { value["cooldownUntil"] = json!(runtime.cooldown_until); }
                if let Some(at) = runtime.last_selected_at { value["lastSelectedAt"] = json!(at); }
                if let Some(kind) = runtime.last_failure_kind { value["lastFailureKind"] = json!(kind); }
                let mut cooldowns = serde_json::Map::new();
                for ((provider, model, id), health) in &state.model_runtime {
                    if provider == &registration.id && id == &account.account.id && health.cooldown_until > now {
                        cooldowns.insert(model.clone(), json!(health.cooldown_until));
                    }
                }
                if !cooldowns.is_empty() { value["modelCooldowns"] = cooldowns.into(); }
                value
            }).collect();
            let bias = state
                .providers
                .get(&registration.id)
                .map_or(SelectionBias::default(), |(_, r)| r.selection_bias);
            let mut value = json!({"id":registration.id,"label":registration.label,"policy":pool.policy,"affinity":pool.affinity,"firstAccountBias":bias == SelectionBias::FirstAccount,"accounts":accounts});
            if let Some(hint) = &registration.management_hint {
                value["managementHint"] = json!(hint);
            }
            providers.push(value);
        }
        Ok(json!({"providers":providers}))
    }
    pub fn pool_preference(&self, id: &str) -> Result<PoolPreference, Error> {
        self.capture(id).map(|(_, pool)| pool)
    }
    pub async fn update_pool(&self, id: &str, patch: PoolPatch) -> Result<Value, Error> {
        {
            let mut state = lock(&self.0);
            state.registration(id)?;
            let mut pool = state.pool(id);
            if let Some(policy) = patch.policy {
                pool.policy = policy;
            }
            if let Some(affinity) = patch.affinity {
                pool.affinity = affinity;
            }
            if let Some(accounts) = patch.accounts {
                pool.accounts = accounts;
            }
            state.preferences.insert(id.into(), pool);
        }
        self.snapshot().await?["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pool| pool["id"] == id)
            .cloned()
            .ok_or_else(|| Error::UnknownProvider(id.into()))
    }
    pub fn errors_before_switch(&self) -> f64 {
        lock(&self.0)
            .options
            .settings
            .errors_before_switch
            .floor()
            .max(1.0)
    }
    pub fn update_settings(&self, patch: &Value) -> Result<(), Error> {
        let mut state = lock(&self.0);
        let settings = &mut state.options.settings;
        for (key, slot) in [
            ("rateLimitCooldownMs", &mut settings.rate_limit_cooldown_ms),
            ("quotaCooldownMs", &mut settings.quota_cooldown_ms),
            ("authCooldownMs", &mut settings.auth_cooldown_ms),
            (
                "transientBaseCooldownMs",
                &mut settings.transient_base_cooldown_ms,
            ),
            ("maxCooldownMs", &mut settings.max_cooldown_ms),
            ("errorsBeforeSwitch", &mut settings.errors_before_switch),
        ] {
            if let Some(value) = patch.get(key) {
                *slot = value.as_f64().filter(|value| value.is_finite() && *value >= 0.0).ok_or_else(|| Error::Message(format!("multiprovider: scheduler setting \"{key}\" must be a non-negative number")))?;
            }
        }
        Ok(())
    }
    pub fn reset_health(&self, id: &str, account: &str) -> Result<(), Error> {
        let mut state = lock(&self.0);
        state.registration(id)?;
        if let Some(runtime) = state.runtime.get(&(id.into(), account.into())) {
            clear_health(&mut lock(runtime));
        }
        state
            .model_runtime
            .retain(|(provider, _, candidate), _| provider != id || candidate != account);
        Ok(())
    }
    pub async fn pin_account(&self, id: &str, key: &str, account: &str) -> Result<(), Error> {
        let (registration, pool) = self.capture(id)?;
        let accounts = self.effective(&registration, &pool).await?;
        let target = accounts
            .iter()
            .find(|candidate| candidate.account.id == account)
            .ok_or_else(|| Error::UnknownAccount(id.into(), account.into()))?;
        if !target.enabled {
            return Err(Error::Message(format!(
                "multiprovider: account \"{}\" is disabled",
                target.account.label
            )));
        }
        lock(&self.0).affinity.entry(id.into()).or_default().insert(
            key.into(),
            AffinityPin {
                account_id: account.into(),
                explicit: true,
            },
        );
        Ok(())
    }
    pub fn affinity(&self, id: &str, key: &str) -> Result<Option<AffinityPin>, Error> {
        let state = lock(&self.0);
        state.registration(id)?;
        Ok(state
            .affinity
            .get(id)
            .and_then(|pins| pins.get(key))
            .cloned())
    }
    pub fn clear_affinity(&self, id: Option<&str>, key: Option<&str>) {
        let mut state = lock(&self.0);
        match (id, key) {
            (None, _) => state.affinity.clear(),
            (Some(id), None) => {
                state.affinity.remove(id);
            }
            (Some(id), Some(key)) => {
                if let Some(pins) = state.affinity.get_mut(id) {
                    pins.remove(key);
                }
            }
        }
    }
}

impl State {
    fn registration(&self, id: &str) -> Result<Arc<Registration>, Error> {
        self.providers
            .get(id)
            .map(|(_, registration)| registration.clone())
            .ok_or_else(|| Error::UnknownProvider(id.into()))
    }
    fn pool(&self, id: &str) -> PoolPreference {
        self.preferences
            .get(id)
            .cloned()
            .unwrap_or_else(|| PoolPreference {
                provider_id: id.into(),
                policy: self.options.default_policy,
                affinity: self.options.affinity,
                accounts: Vec::new(),
            })
    }
    fn runtime_for(&mut self, id: &str, account: &str) -> Arc<Mutex<Health>> {
        self.runtime
            .entry((id.into(), account.into()))
            .or_default()
            .clone()
    }
    fn blocking(&self, options: &AcquireOptions, account: &EffectiveAccount, now: f64) -> Vec<f64> {
        let mut blocking = Vec::new();
        let until = lock(&account.runtime).cooldown_until;
        if until > now {
            blocking.push(until);
        }
        if let Some(model) = &options.model_id {
            if let Some(health) = self.model_runtime.get(&(
                options.provider_id.clone(),
                model.clone(),
                account.account.id.clone(),
            )) {
                if health.cooldown_until > now {
                    blocking.push(health.cooldown_until);
                }
            }
        }
        blocking
    }
    fn select(
        &mut self,
        id: &str,
        policy: SelectionPolicy,
        accounts: &[&EffectiveAccount],
    ) -> Result<usize, Error> {
        if matches!(
            policy,
            SelectionPolicy::LeastInflight | SelectionPolicy::Priority
        ) {
            let mut selected = 0;
            for index in 1..accounts.len() {
                let left = lock(&accounts[index].runtime);
                let right = lock(&accounts[selected].runtime);
                let priority = if policy == SelectionPolicy::Priority {
                    accounts[index]
                        .priority
                        .partial_cmp(&accounts[selected].priority)
                        .unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    std::cmp::Ordering::Equal
                };
                if priority
                    .then(left.in_flight.cmp(&right.in_flight))
                    .then(
                        left.last_selected_at
                            .unwrap_or(0.0)
                            .partial_cmp(&right.last_selected_at.unwrap_or(0.0))
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
                    .is_lt()
                {
                    selected = index;
                }
            }
            return Ok(selected);
        }
        let bias = self
            .providers
            .get(id)
            .map_or(SelectionBias::default(), |(_, provider)| {
                provider.selection_bias
            });
        if policy == SelectionPolicy::RoundRobin && bias == SelectionBias::FirstAccount {
            return Ok(0);
        }
        if policy == SelectionPolicy::WeightedRoundRobin
            || accounts
                .iter()
                .any(|account| account.weight != accounts[0].weight)
        {
            let total: f64 = accounts.iter().map(|account| account.weight.max(1.0)).sum();
            if !self.scores.contains_key(id) {
                self.scores.insert(id.into(), HashMap::new());
                let warmup = if total > 1.0 {
                    (self.options.random_int)(total)?
                } else {
                    0
                };
                for _ in 0..warmup {
                    weighted_step(self.scores.get_mut(id).unwrap(), accounts, total);
                }
            }
            let scores = self.scores.get_mut(id).unwrap();
            scores.retain(|id, _| accounts.iter().any(|account| &account.account.id == id));
            return Ok(weighted_step(scores, accounts, total));
        }
        if !self.cursor.contains_key(id) {
            let cursor = if accounts.len() > 1 {
                (self.options.random_int)(accounts.len() as f64)?
            } else {
                0
            };
            self.cursor.insert(id.into(), cursor);
        }
        let cursor = self.cursor.get_mut(id).unwrap();
        let selected = (*cursor % accounts.len() as u64) as usize;
        *cursor = (*cursor + 1) % accounts.len() as u64;
        Ok(selected)
    }
}
fn weighted_step(
    scores: &mut HashMap<String, f64>,
    accounts: &[&EffectiveAccount],
    total: f64,
) -> usize {
    let mut selected = 0;
    let mut best = f64::NEG_INFINITY;
    for (index, account) in accounts.iter().enumerate() {
        let score = scores.entry(account.account.id.clone()).or_default();
        *score += account.weight.max(1.0);
        if *score > best {
            best = *score;
            selected = index;
        }
    }
    *scores.get_mut(&accounts[selected].account.id).unwrap() -= total;
    selected
}
fn clear_health(health: &mut Health) {
    health.consecutive_failures = 0;
    health.cooldown_until = 0.0;
    health.last_failure_kind = None;
}

pub struct Lease {
    pub id: String,
    pub provider_id: String,
    pub account: Account,
    pub acquired_at: f64,
    model_id: Option<String>,
    runtime: Option<Arc<Mutex<Health>>>,
    registration: Arc<Registration>,
    service: Service,
}
impl Lease {
    pub fn release(&mut self, outcome: LeaseOutcome) -> Option<Disposition> {
        let runtime = self.runtime.take()?;
        {
            let mut health = lock(&runtime);
            health.in_flight = health.in_flight.saturating_sub(1);
        }
        // Classifiers are extension code: never invoke them while holding the scheduler lock.
        let disposition = match &outcome {
            LeaseOutcome::Failure { error } => Some(match &self.registration.classify_failure {
                Some(classifier) => match classifier(error, &self.account) {
                    Ok(disposition) => disposition.unwrap_or_else(|| default_disposition(error)),
                    Err(_) => Disposition {
                        kind: FailureKind::Fatal,
                        retryable: false,
                        cooldown_ms: None,
                        scope: None,
                    },
                },
                None => default_disposition(error),
            }),
            _ => None,
        };
        let mut state = lock(&self.service.0);
        if matches!(outcome, LeaseOutcome::Success) {
            clear_health(&mut lock(
                &state.runtime_for(&self.provider_id, &self.account.id),
            ));
            if let Some(model) = &self.model_id {
                state.model_runtime.shift_remove(&(
                    self.provider_id.clone(),
                    model.clone(),
                    self.account.id.clone(),
                ));
            }
        }
        if let Some(disposition) = &disposition {
            let now = (state.options.now)();
            let settings = state.options.settings.clone();
            let update = |health: &mut Health| {
                health.consecutive_failures += 1;
                health.last_failure_kind = Some(disposition.kind);
                let cooldown = disposition
                    .cooldown_ms
                    .unwrap_or_else(|| match disposition.kind {
                        FailureKind::RateLimit => settings.rate_limit_cooldown_ms,
                        FailureKind::Quota => settings.quota_cooldown_ms,
                        FailureKind::Auth => settings.auth_cooldown_ms,
                        FailureKind::Transient => {
                            settings.transient_base_cooldown_ms
                                * 2f64.powi((health.consecutive_failures - 1).min(10) as i32)
                        }
                        FailureKind::Fatal => 0.0,
                    });
                health.cooldown_until = health
                    .cooldown_until
                    .max(now + cooldown.max(0.0).min(settings.max_cooldown_ms));
            };
            if let (Some(FailureScope::AccountModel), Some(model)) =
                (disposition.scope, &self.model_id)
            {
                update(
                    state
                        .model_runtime
                        .entry((
                            self.provider_id.clone(),
                            model.clone(),
                            self.account.id.clone(),
                        ))
                        .or_default(),
                );
            } else {
                update(&mut lock(
                    &state.runtime_for(&self.provider_id, &self.account.id),
                ));
            }
        }
        disposition
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.release(LeaseOutcome::Cancelled);
    }
}

pub fn default_disposition(failure: &AttemptFailure) -> Disposition {
    static PATTERNS: OnceLock<[Regex; 5]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        // JavaScript's non-Unicode regex uses ASCII digits/boundaries and a different space/dot set.
        let space = r"[\t\n\x0B\x0C\r \u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";
        let dot = r"[^\r\n\u{2028}\u{2029}]";
        let unit = r"[^\r\n\u{2028}\u{2029}\u{10000}-\u{10FFFF}]";
        [
            Regex::new(&format!(r"^(?:http{space}*)?([0-9]{{3}})(?::|{space}|$)")).unwrap(),
            Regex::new(&format!(r"(?-u:\b)status(?:{space}+code)?{space}*[:=]?{space}*([0-9]{{3}})(?-u:\b)")).unwrap(),
            Regex::new(&format!(r"rate{unit}?limit|too many requests|overloaded")).unwrap(),
            Regex::new(&format!(r"quota|usage{unit}?limit|limit{dot}*reached|out of credits")).unwrap(),
            Regex::new(&format!(r"unauthorized|forbidden|invalid{dot}*(?:token|api{unit}?key|auth|grant)|(?:token|credential){dot}*(?:expired|invalid)")).unwrap(),
        ]
    });
    let ascii = failure.message.to_ascii_lowercase();
    let status = failure.status.or_else(|| {
        patterns[0]
            .captures(&ascii)
            .or_else(|| patterns[1].captures(&ascii))
            .and_then(|capture| capture[1].parse().ok())
    });
    let message = failure.message.to_lowercase();
    let kind = if status == Some(429) || patterns[2].is_match(&message) {
        FailureKind::RateLimit
    } else if status == Some(402) || patterns[3].is_match(&message) {
        FailureKind::Quota
    } else if matches!(status, Some(401 | 403)) || patterns[4].is_match(&message) {
        FailureKind::Auth
    } else if status.is_some_and(|status| status >= 500 || status == 408 || status == 425) {
        FailureKind::Transient
    } else {
        FailureKind::Fatal
    };
    Disposition {
        kind,
        retryable: kind != FailureKind::Fatal,
        cooldown_ms: None,
        scope: None,
    }
}
