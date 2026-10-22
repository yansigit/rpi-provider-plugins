//! The extension's provider-composition layer.
//!
//! Pi loads one instance of the extension per session, and subagent sessions
//! share the parent's provider registry. Each instance lifts the real base
//! provider, never another instance's lift, so shutting one instance down
//! cannot break a chain of wrappers. This module ports the decisions that make
//! that work: `isLiftOf` through Pi's composed wrapper, `realBase` behind any
//! chain of lifts, the process-wide list of live registrations, and what a
//! departing session hands the registry back to in whatever order sessions end.
use crate::virtual_provider::{
    capture_virtual_model_template, heal_virtual_templates, VirtualProviderConfig,
};
use indexmap::{IndexMap, IndexSet};
use serde_json::{json, Value};
use std::collections::HashMap;

/// A provider as the extension sees it: a stable identity, an optional headers
/// token shared by reference with its lift, the real base a lift wraps, and the
/// models it advertises (virtual-template healing reads them).
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub headers: Option<u64>,
    pub base: Option<usize>,
    pub lift: bool,
    pub models: Vec<Value>,
}

#[derive(Default)]
pub struct Graph {
    nodes: Vec<Node>,
    next_headers: u64,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn node(&self, index: usize) -> &Node {
        &self.nodes[index]
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn add_base(&mut self, id: &str, models: Vec<Value>) -> usize {
        self.nodes.push(Node {
            id: id.into(),
            headers: None,
            base: None,
            lift: false,
            models,
        });
        self.nodes.len() - 1
    }
    /// A lift gets its own headers object so `is_lift_of` sees through Pi's composition.
    pub fn add_lift(&mut self, base: usize) -> usize {
        let models = self.nodes[base].models.clone();
        let id = self.nodes[base].id.clone();
        self.next_headers += 1;
        let headers = self.next_headers;
        self.nodes.push(Node {
            id,
            headers: Some(headers),
            base: Some(base),
            lift: true,
            models,
        });
        self.nodes.len() - 1
    }
    /// Pi's freshly composed wrapper: a new object that shares the wrapped
    /// provider's headers by reference and delegates everything else.
    pub fn add_composed(&mut self, inner: usize) -> usize {
        let node = self.nodes[inner].clone();
        self.nodes.push(Node {
            id: node.id,
            headers: node.headers,
            base: None,
            lift: false,
            models: node.models,
        });
        self.nodes.len() - 1
    }
    pub fn add_virtual(&mut self, id: &str, models: Vec<Value>) -> usize {
        self.nodes.push(Node {
            id: id.into(),
            headers: None,
            base: None,
            lift: false,
            models,
        });
        self.nodes.len() - 1
    }
    /// `isLiftOf`: same object, or a wrapper sharing the lift's headers object.
    pub fn is_lift_of(&self, current: Option<usize>, lift: Option<usize>) -> bool {
        match (current, lift) {
            (Some(current), Some(lift)) => {
                current == lift
                    || (self.nodes[lift].headers.is_some()
                        && self.nodes[current].headers == self.nodes[lift].headers)
            }
            _ => false,
        }
    }
    /// `realBase`: walk tagged lifts, and untagged composed wrappers matched
    /// against the live lifts, down to the provider the extension must lift.
    pub fn real_base(&self, live: &Live, id: &str, provider: Option<usize>) -> Option<usize> {
        let mut current = provider;
        while let Some(candidate) = current {
            let lift = if self.nodes[candidate].lift {
                Some(candidate)
            } else {
                live.get(id).iter().copied().find(|live| {
                    self.nodes[*live].lift && self.is_lift_of(Some(candidate), Some(*live))
                })
            };
            match lift {
                Some(lift) => current = self.nodes[lift].base,
                None => break,
            }
        }
        current
    }
}

/// The process-wide list of live registrations (lifts and virtual providers).
#[derive(Default)]
pub struct Live {
    map: HashMap<String, Vec<usize>>,
}

impl Live {
    pub fn get(&self, id: &str) -> &[usize] {
        self.map.get(id).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn replace(
        &mut self,
        id: &str,
        previous: Option<usize>,
        next: Option<usize>,
    ) -> Option<usize> {
        let list = self.map.entry(id.to_owned()).or_default();
        if let Some(previous) = previous {
            list.retain(|node| *node != previous);
        }
        if let Some(next) = next {
            list.push(next);
        }
        let last = list.last().copied();
        if list.is_empty() {
            self.map.remove(id);
        }
        last
    }
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

/// One extension instance's private install state.
#[derive(Default)]
pub struct Session {
    pub base_providers: HashMap<String, usize>,
    pub installed: HashMap<String, usize>,
    pub managed: IndexSet<String>,
    pub managed_bases: HashMap<String, usize>,
    pub warned_missing: IndexSet<String>,
    pub virtuals: IndexMap<String, usize>,
    pub healed: IndexSet<String>,
    /// `currentModelRegistry` is only captured once a session context exists.
    pub context_ready: bool,
}

/// `registerProvider` classification as the reference records it.
fn classify(graph: &Graph, node: usize) -> Value {
    let node = graph.node(node);
    json!({
        "id": node.id,
        "isLift": node.lift,
        "liftedBaseId": node.base.map(|base| graph.node(base).id.clone()),
        "hasHeaders": node.headers.is_some(),
    })
}

/// The shared provider registry plus the process-wide live registrations.
pub struct Composition {
    pub graph: Graph,
    pub live: Live,
    native: HashMap<String, usize>,
    view: HashMap<String, usize>,
    composed: bool,
    pub trace: Vec<Value>,
    pub notifications: Vec<(String, String)>,
}

impl Composition {
    pub fn new(composed: bool) -> Self {
        Self {
            graph: Graph::new(),
            live: Live::default(),
            native: HashMap::new(),
            view: HashMap::new(),
            composed,
            trace: Vec::new(),
            notifications: Vec::new(),
        }
    }

    /// The base provider already registered by Pi before the extension loads.
    pub fn add_base(&mut self, id: &str, models: Vec<Value>) {
        let node = self.graph.add_base(id, models);
        self.native.insert(id.into(), node);
        let view = if self.composed {
            self.graph.add_composed(node)
        } else {
            node
        };
        self.view.insert(id.into(), view);
    }

    /// What `ctx.modelRegistry.getProvider(id)` returns.
    pub fn get(&self, id: &str) -> Option<usize> {
        self.view.get(id).copied()
    }

    /// What Pi's registry actually holds (the registered, not composed, object).
    pub fn native(&self, id: &str) -> Option<usize> {
        self.native.get(id).copied()
    }

    pub fn has(&self, id: &str) -> bool {
        self.view.contains_key(id)
    }

    /// `pi.registerProvider`.
    pub fn register(&mut self, node: usize) -> usize {
        let id = self.graph.node(node).id.clone();
        self.native.insert(id.clone(), node);
        let view = if self.composed {
            self.graph.add_composed(node)
        } else {
            node
        };
        self.view.insert(id, view);
        let mut entry = classify(&self.graph, node);
        entry["op"] = json!("register");
        self.trace.push(entry);
        node
    }

    /// `pi.unregisterProvider`.
    pub fn unregister(&mut self, id: &str) {
        self.native.remove(id);
        self.view.remove(id);
        self.trace.push(json!({"op": "unregister", "id": id}));
    }

    pub fn classify(&self, node: Option<usize>) -> Value {
        match node {
            Some(node) => classify(&self.graph, node),
            None => Value::Null,
        }
    }

    /// Extension load: stored virtual providers are registered before
    /// `session_start` so Pi can resolve resumed session model patterns.
    pub fn load(&mut self, session: &mut Session, virtuals: &mut [VirtualProviderConfig]) {
        self.refresh_virtual(session, virtuals);
    }

    fn resolve_template(
        &self,
        session: &Session,
        provider_id: &str,
        model_id: &str,
    ) -> Option<Value> {
        let node = session
            .installed
            .get(provider_id)
            .copied()
            .or_else(|| session.base_providers.get(provider_id).copied())
            .or_else(|| self.get(provider_id))?;
        self.graph
            .node(node)
            .models
            .iter()
            .find(|model| model["id"].as_str() == Some(model_id))
            .map(capture_virtual_model_template)
    }

    /// `refreshVirtual`: heal a template-less config once a live backing model
    /// resolves, persist the healed config (the shared store, so later sessions
    /// load it already healed), and re-register the config that changed.
    pub fn refresh_virtual(
        &mut self,
        session: &mut Session,
        virtuals: &mut [VirtualProviderConfig],
    ) {
        for index in 0..virtuals.len() {
            let config = virtuals[index].clone();
            match session.virtuals.get(&config.id).copied() {
                Some(current) => {
                    if !session.context_ready || session.healed.contains(&config.id) {
                        continue;
                    }
                    let healed = {
                        let this = &*self;
                        let session = &*session;
                        heal_virtual_templates(&config, |provider_id, model_id| {
                            this.resolve_template(session, provider_id, model_id)
                        })
                    };
                    let Some(healed) = healed else { continue };
                    session.healed.insert(config.id.clone());
                    virtuals[index] = healed;
                    let node = self.graph.add_virtual(&config.id, Vec::new());
                    self.live.replace(&config.id, Some(current), Some(node));
                    session.virtuals.insert(config.id.clone(), node);
                    self.register(node);
                }
                None => {
                    let node = self.graph.add_virtual(&config.id, Vec::new());
                    self.live.replace(&config.id, None, Some(node));
                    session.virtuals.insert(config.id.clone(), node);
                    self.register(node);
                }
            }
        }
    }

    /// `refreshManaged`: create the managed integrations whose base resolves,
    /// and drop the ones the store no longer lists.
    fn refresh_managed(&mut self, session: &mut Session, stored: &[String]) {
        for id in session.managed.clone() {
            if stored.contains(&id) {
                continue;
            }
            session.managed.shift_remove(&id);
            session.managed_bases.remove(&id);
            self.restore(session, &id);
        }
        for id in stored {
            let current = self.get(id);
            let prior = session.installed.get(id).copied();
            let choice = if self.graph.is_lift_of(current, prior) {
                session.base_providers.get(id).copied()
            } else {
                current
            };
            let Some(base) = self.graph.real_base(&self.live, id, choice) else {
                continue;
            };
            if session.managed_bases.get(id) != Some(&base) {
                session.managed_bases.insert(id.clone(), base);
                session.managed.insert(id.clone());
            }
        }
    }

    /// `restoreProvider`, with a live `ctx` (the only caller shape these
    /// sessions use): hand the registry back only when it still returns this
    /// instance's lift.
    fn restore(&mut self, session: &mut Session, id: &str) {
        let base = session.base_providers.get(id).copied();
        let installed = session.installed.get(id).copied();
        let current = self.get(id);
        let newest = self.live.replace(id, installed, None);
        if base.is_some() && self.graph.is_lift_of(current, installed) {
            if let Some(node) = newest.or(base) {
                self.register(node);
            }
        }
        session.installed.remove(id);
        session.base_providers.remove(id);
    }

    /// `install`: lift the real base, never this or another instance's lift.
    fn install(&mut self, session: &mut Session, id: &str) {
        if !session.managed.contains(id) {
            self.restore(session, id);
            return;
        }
        let current = self.get(id);
        let prior = session.installed.get(id).copied();
        let choice = if self.graph.is_lift_of(current, prior) {
            session.base_providers.get(id).copied()
        } else {
            current
        };
        let Some(base) = self.graph.real_base(&self.live, id, choice) else {
            if session.warned_missing.insert(id.to_owned()) {
                self.notifications.push((
                    format!("multiprovider: provider \"{id}\" is not registered"),
                    "warning".into(),
                ));
            }
            return;
        };
        session.warned_missing.shift_remove(id);
        if self.graph.is_lift_of(current, prior) && session.base_providers.get(id) == Some(&base) {
            return;
        }
        let lifted = self.graph.add_lift(base);
        self.live.replace(id, prior, Some(lifted));
        self.register(lifted);
        session.base_providers.insert(id.to_owned(), base);
        session.installed.insert(id.to_owned(), lifted);
    }

    /// `reconcile`: refresh, manage, install, then apply recorded pins. Pins do
    /// not touch the registry, so the composition trace is unaffected by them.
    pub fn reconcile(&mut self, session: &mut Session, virtuals: &mut [VirtualProviderConfig], stored: &[String]) {
        session.context_ready = true;
        self.refresh_virtual(session, virtuals);
        self.refresh_managed(session, stored);
        let mut ids: IndexSet<String> = session.managed.clone();
        for id in session.installed.keys() {
            ids.insert(id.clone());
        }
        for id in ids {
            self.install(session, &id);
        }
        self.refresh_virtual(session, virtuals);
    }

    /// `session_shutdown`: restore every installed provider, then hand the
    /// registry back to the newest live virtual provider (or unregister it).
    pub fn shutdown(&mut self, session: &mut Session) {
        let installed: Vec<String> = session.installed.keys().cloned().collect();
        for id in installed {
            self.restore(session, &id);
        }
        session.managed.clear();
        session.managed_bases.clear();
        let virtuals: Vec<(String, usize)> = session
            .virtuals
            .iter()
            .map(|(id, node)| (id.clone(), *node))
            .collect();
        for (id, provider) in virtuals {
            let newest = self.live.replace(&id, Some(provider), None);
            if self.get(&id) != Some(provider) {
                continue;
            }
            match newest {
                Some(node) => {
                    self.register(node);
                }
                None => self.unregister(&id),
            }
        }
        session.virtuals.clear();
        session.healed.clear();
        session.context_ready = false;
    }
}
