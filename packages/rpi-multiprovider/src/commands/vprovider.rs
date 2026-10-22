//! `extensions/multiprovider.ts` `vprovider` handler and its inline
//! `VirtualProviderEditorDialog`, projected onto native dialog controls.
//!
//! Every editor page is a `settings` control whose rows cycle to their own id
//! (`menuItem` sets `values: [id]`), except the create/edit inputs, which are
//! `input` controls. The page error the upstream dialog renders as an extra
//! warning line is appended to the control's description after a newline. The
//! theme colors the backend rows carry are dropped, so labels are plain text.
use super::port::{confirm, notify, Port};
use super::settings::{self, SettingItem};
use super::Ui;
use crate::types::trim;
use crate::virtual_provider::capture_virtual_model_template;
use serde_json::{json, Value};

/// One model of a provider the editor can use as a backing model.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateModel {
    pub id: String,
    pub name: String,
    /// The host's model object, as `captureVirtualModelTemplate` reads it.
    pub model: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CandidateProvider {
    pub id: String,
    pub name: String,
    pub models: Vec<CandidateModel>,
}

/// What `uniqueProviders(ctx, baseProviders)` and
/// `ctx.modelRegistry.getProvider(id)` supply. `Port` covers the store,
/// `reconcile` and the virtual-provider registry; the model catalog is the one
/// extra input the editor needs.
pub trait VproviderSources: Send + Sync {
    /// Every provider Pi or the extension knows, with the models each exposes.
    fn providers_with_models(&self) -> Vec<CandidateProvider>;
    /// `ctx.modelRegistry.getProvider(id) !== undefined`.
    fn provider_registered(&self, id: &str) -> bool;
}

/// `VirtualEditorOutcome`.
#[derive(Clone, Debug, PartialEq)]
pub enum EditorOutcome {
    Dismissed,
    Saved(Value),
    Discarded,
    Removed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorPage {
    Root,
    Menu,
    ProviderPicker,
    ModelPicker,
    Backend,
    Input(InputPurpose),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputPurpose {
    ProviderId,
    ModelId,
    /// Unreachable through the editor: the backend "Set weight" row opens this
    /// page and the trailing `goTo(menu)` immediately overrides it (an upstream
    /// quirk, preserved). Kept so the port mirrors the upstream branch.
    #[allow(dead_code)]
    Weight,
}

/// `VirtualProviderEditorDialog`'s state.
struct Editor {
    stored: Vec<Value>,
    candidates: Vec<CandidateProvider>,
    draft: Option<Value>,
    page: EditorPage,
    input_back: EditorPage,
    create_provider_id: Option<String>,
    chosen_provider: Option<String>,
    active_backend: usize,
    input_initial: String,
    page_error: String,
}

impl Editor {
    fn new(stored: Vec<Value>, candidates: Vec<CandidateProvider>, draft: Option<Value>) -> Self {
        let page = if draft.is_none() { EditorPage::Root } else { EditorPage::Menu };
        Self {
            stored,
            candidates,
            draft,
            page,
            input_back: EditorPage::Root,
            create_provider_id: None,
            chosen_provider: None,
            active_backend: 0,
            input_initial: String::new(),
            page_error: String::new(),
        }
    }
    /// `goTo(page)`: navigation clears the page error.
    fn go_to(&mut self, page: EditorPage) {
        self.page = page;
        self.page_error.clear();
    }
    fn with_error(&self, description: &str) -> String {
        if self.page_error.is_empty() {
            description.to_owned()
        } else {
            format!("{description}\n{}", self.page_error)
        }
    }
    fn model(&self) -> &Value {
        &self.draft.as_ref().expect("the editor draft")["models"][0]
    }
    fn model_mut(&mut self) -> &mut Value {
        &mut self.draft.as_mut().expect("the editor draft")["models"][0]
    }
    fn backends(&self) -> Vec<Value> {
        self.model()["backends"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

/// `VIRTUAL_ID_PATTERN`: letters, numbers, dots, dashes, underscores, starting
/// with a letter or number.
fn valid_virtual_id(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn menu_item(id: impl Into<String>, label: impl Into<String>) -> SettingItem {
    let id = id.into();
    SettingItem::new(id.clone(), label, "").cycling_owned(vec![id])
}

fn separator_item(id: &str) -> SettingItem {
    SettingItem::new(id, "", "")
}

fn first_selected(items: &[SettingItem]) -> Option<&str> {
    items.first().map(|item| item.id.as_str())
}

fn change_id(reply: &Value) -> Result<String, String> {
    if reply["type"] != "change" {
        return Err("unexpected reply to a settings control".to_owned());
    }
    reply["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "setting change requires an ID".to_owned())
}

fn backend_label(index: usize, backend: &Value) -> String {
    let enabled = backend["enabled"] != json!(false);
    format!(
        "{}. {} · {} · {} · w{}",
        index + 1,
        backend["providerId"].as_str().unwrap_or(""),
        backend["modelId"].as_str().unwrap_or(""),
        if enabled { "enabled" } else { "disabled" },
        settings::js_number(backend["weight"].as_f64().unwrap_or(1.0))
    )
}

async fn run_editor<U: Ui, F: Fn(&str) -> bool>(
    ui: &U,
    editor: &mut Editor,
    available: &F,
) -> Result<EditorOutcome, String> {
    loop {
        match editor.page {
            EditorPage::Root => {
                let mut items = vec![menu_item("create", "Create new virtual provider")];
                for config in &editor.stored {
                    let id = config["id"].as_str().unwrap_or("");
                    items.push(menu_item(format!("edit-{id}"), format!("Edit {id}")));
                }
                for config in &editor.stored {
                    let id = config["id"].as_str().unwrap_or("");
                    items.push(menu_item(format!("delete-{id}"), format!("Delete {id}")));
                }
                let control = settings::settings_control(
                    "Virtual providers",
                    &editor.with_error("Create, edit, or remove virtual providers that map one model across provider models."),
                    &items,
                    "",
                    first_selected(&items),
                );
                let Some(reply) = ui.show(control).await? else {
                    return Ok(EditorOutcome::Dismissed);
                };
                let id = change_id(&reply)?;
                if id == "create" {
                    editor.create_provider_id = None;
                    editor.input_back = EditorPage::Root;
                    editor.input_initial = "pooled".to_owned();
                    editor.go_to(EditorPage::Input(InputPurpose::ProviderId));
                } else if let Some(target) = id.strip_prefix("edit-") {
                    if let Some(config) = editor
                        .stored
                        .iter()
                        .find(|config| config["id"].as_str() == Some(target))
                    {
                        editor.draft = Some(config.clone());
                        editor.go_to(EditorPage::Menu);
                    }
                } else if let Some(target) = id.strip_prefix("delete-") {
                    return Ok(EditorOutcome::Removed(target.to_owned()));
                }
            }
            EditorPage::Menu => {
                let draft = editor
                    .draft
                    .clone()
                    .ok_or_else(|| "the editor menu needs a draft".to_owned())?;
                let model_id = draft["models"][0]["id"].as_str().unwrap_or("").to_owned();
                let mut items = vec![
                    menu_item("model-id", format!("Model id: {model_id}")),
                    menu_item("add", "Add backing provider model"),
                    separator_item("sep-top"),
                ];
                for (index, backend) in editor.backends().iter().enumerate() {
                    items.push(menu_item(format!("backend-{index}"), backend_label(index, backend)));
                }
                items.push(separator_item("sep-bottom"));
                items.push(menu_item("save", "Save and apply"));
                items.push(menu_item("discard", "Discard changes"));
                let title = format!(
                    "Virtual provider \"{}\"",
                    draft["id"].as_str().unwrap_or("")
                );
                let control = settings::settings_control(
                    &title,
                    &editor.with_error("Enter selects · Esc discards changes."),
                    &items,
                    "",
                    first_selected(&items),
                );
                let Some(reply) = ui.show(control).await? else {
                    return Ok(EditorOutcome::Discarded);
                };
                let id = change_id(&reply)?;
                match id.as_str() {
                    "model-id" => {
                        editor.input_back = EditorPage::Menu;
                        editor.input_initial = model_id;
                        editor.go_to(EditorPage::Input(InputPurpose::ModelId));
                    }
                    "add" => editor.go_to(EditorPage::ProviderPicker),
                    "save" => {
                        if editor
                            .backends()
                            .iter()
                            .filter(|backend| backend["enabled"] != json!(false))
                            .count()
                            == 0
                        {
                            editor.page_error =
                                "Add at least one enabled backing provider model before saving."
                                    .to_owned();
                            continue;
                        }
                        // Heal templates for backends stored before capture
                        // existed; live resolution still wins at runtime.
                        for index in 0..editor.backends().len() {
                            let backend = editor.backends()[index].clone();
                            if backend["enabled"] == json!(false) || !backend["template"].is_null() {
                                continue;
                            }
                            let provider_id = backend["providerId"].as_str().unwrap_or("");
                            let model_id = backend["modelId"].as_str().unwrap_or("");
                            let candidate = editor
                                .candidates
                                .iter()
                                .find(|provider| provider.id == provider_id)
                                .and_then(|provider| {
                                    provider.models.iter().find(|model| model.id == model_id)
                                });
                            if let Some(candidate) = candidate {
                                let template = capture_virtual_model_template(&candidate.model);
                                editor.model_mut()["backends"][index]["template"] = template;
                            }
                        }
                        return Ok(EditorOutcome::Saved(
                            editor.draft.clone().expect("the editor draft"),
                        ));
                    }
                    "discard" => return Ok(EditorOutcome::Discarded),
                    other if other.starts_with("backend-") => {
                        editor.active_backend = other["backend-".len()..].parse().unwrap_or(0);
                        editor.go_to(EditorPage::Backend);
                    }
                    _ => {}
                }
            }
            EditorPage::ProviderPicker => {
                let items: Vec<SettingItem> = editor
                    .candidates
                    .iter()
                    .map(|provider| menu_item(provider.id.clone(), format!("{} ({})", provider.name, provider.id)))
                    .collect();
                let control = settings::settings_control(
                    "Backing provider",
                    &editor.with_error("Type to search · Enter picks the provider · Esc goes back."),
                    &items,
                    "",
                    first_selected(&items),
                );
                let Some(reply) = ui.show(control).await? else {
                    editor.go_to(EditorPage::Menu);
                    continue;
                };
                let id = change_id(&reply)?;
                if editor.candidates.iter().any(|provider| provider.id == id) {
                    editor.chosen_provider = Some(id);
                    editor.go_to(EditorPage::ModelPicker);
                }
            }
            EditorPage::ModelPicker => {
                let provider = editor
                    .chosen_provider
                    .as_ref()
                    .and_then(|id| editor.candidates.iter().find(|provider| &provider.id == id))
                    .cloned()
                    .ok_or_else(|| "the model picker needs a chosen provider".to_owned())?;
                let items: Vec<SettingItem> = provider
                    .models
                    .iter()
                    .map(|model| {
                        SettingItem::new(model.id.clone(), model.id.clone(), model.name.clone())
                            .cycling_owned(vec![model.id.clone()])
                    })
                    .collect();
                let control = settings::settings_control(
                    &format!("Backing model for {}", provider.name),
                    &editor.with_error("Type to search · Enter picks the model · Esc goes back."),
                    &items,
                    "",
                    first_selected(&items),
                );
                let Some(reply) = ui.show(control).await? else {
                    editor.go_to(EditorPage::Menu);
                    continue;
                };
                let id = change_id(&reply)?;
                let Some(model) = provider.models.iter().find(|model| model.id == id) else {
                    continue;
                };
                let duplicate = editor.backends().iter().any(|backend| {
                    backend["providerId"].as_str() == Some(provider.id.as_str())
                        && backend["modelId"].as_str() == Some(model.id.as_str())
                });
                if duplicate {
                    editor.page_error = "That provider model is already a backend.".to_owned();
                    continue;
                }
                let template = capture_virtual_model_template(&model.model);
                let backend = json!({
                    "providerId": provider.id,
                    "modelId": model.id,
                    "weight": 1,
                    "template": template,
                });
                editor.model_mut()["backends"]
                    .as_array_mut()
                    .ok_or_else(|| "the editor draft has no backends".to_owned())?
                    .push(backend);
                editor.go_to(EditorPage::Menu);
            }
            EditorPage::Backend => {
                let backends = editor.backends();
                let backend = backends
                    .get(editor.active_backend)
                    .cloned()
                    .ok_or_else(|| "the backend page needs a backend".to_owned())?;
                let enabled = backend["enabled"] != json!(false);
                let items = vec![
                    menu_item("toggle", if enabled { "Disable" } else { "Enable" }),
                    menu_item("weight", "Set weight"),
                    menu_item("remove", "Remove"),
                ];
                let title = format!(
                    "{} · {}",
                    backend["providerId"].as_str().unwrap_or(""),
                    backend["modelId"].as_str().unwrap_or("")
                );
                let control = settings::settings_control(
                    &title,
                    &editor.with_error("Enter selects · Esc goes back."),
                    &items,
                    "",
                    first_selected(&items),
                );
                let Some(reply) = ui.show(control).await? else {
                    editor.go_to(EditorPage::Menu);
                    continue;
                };
                let id = change_id(&reply)?;
                match id.as_str() {
                    "toggle" => {
                        let next = backend["enabled"] == json!(false);
                        let index = editor.active_backend;
                        editor.model_mut()["backends"][index]["enabled"] = json!(next);
                        editor.go_to(EditorPage::Menu);
                    }
                    "weight" => {
                        // Upstream quirk: the input page is opened and then
                        // overridden by the trailing `goTo(menu)`.
                        editor.input_back = EditorPage::Menu;
                        editor.input_initial =
                            settings::js_number(backend["weight"].as_f64().unwrap_or(1.0));
                        editor.go_to(EditorPage::Menu);
                    }
                    "remove" => {
                        let index = editor.active_backend;
                        if let Some(backends) = editor.model_mut()["backends"].as_array_mut() {
                            if index < backends.len() {
                                backends.remove(index);
                            }
                        }
                        editor.go_to(EditorPage::Menu);
                    }
                    _ => {}
                }
            }
            EditorPage::Input(purpose) => {
                let (title, integer_min) = match purpose {
                    InputPurpose::ProviderId => ("Virtual provider id", None),
                    InputPurpose::ModelId => ("Virtual model id (shown in /model)", None),
                    InputPurpose::Weight => ("Set weight", Some(1)),
                };
                let control = settings::input_control(
                    title,
                    &editor.with_error("Enter confirms · Esc goes back."),
                    &editor.input_initial,
                    integer_min,
                );
                let Some(reply) = ui.show(control).await? else {
                    let back = editor.input_back;
                    editor.go_to(back);
                    continue;
                };
                let value = settings::input_reply(&reply)?
                    .ok_or_else(|| "input reply requires text".to_owned())?;
                match purpose {
                    InputPurpose::ProviderId => {
                        if !valid_virtual_id(&value) || !available(&value) {
                            editor.page_error = "Provider id is invalid or already registered.".to_owned();
                            editor.input_initial = value;
                            continue;
                        }
                        editor.create_provider_id = Some(value.clone());
                        editor.input_initial = value;
                        editor.go_to(EditorPage::Input(InputPurpose::ModelId));
                    }
                    InputPurpose::ModelId => {
                        if !valid_virtual_id(&value) {
                            editor.page_error =
                                "Use letters, numbers, dots, dashes, or underscores for the model id."
                                    .to_owned();
                            editor.input_initial = value;
                            continue;
                        }
                        if editor.draft.is_none() {
                            let id = editor.create_provider_id.clone().unwrap_or_default();
                            editor.draft = Some(json!({
                                "id": id,
                                "label": id,
                                "models": [{"id": value, "backends": []}],
                            }));
                            editor.page_error = "Add at least one enabled backing provider model, then choose \"Save and apply\"."
                                .to_owned();
                        } else {
                            editor.model_mut()["id"] = json!(value);
                        }
                        editor.go_to(EditorPage::Menu);
                    }
                    InputPurpose::Weight => {
                        let parsed = settings::js_number_parse(&value);
                        let exists = editor
                            .model()["backends"]
                            .as_array()
                            .map(|backends| editor.active_backend < backends.len())
                            .unwrap_or(false);
                        if !(parsed.fract() == 0.0 && parsed.is_finite() && parsed >= 1.0) || !exists {
                            editor.page_error = "Weight must be an integer ≥ 1.".to_owned();
                            editor.input_initial = value;
                            continue;
                        }
                        let index = editor.active_backend;
                        editor.model_mut()["backends"][index]["weight"] = json!(parsed);
                        editor.go_to(EditorPage::Menu);
                    }
                }
            }
        }
    }
}

/// `uniqueProviders(...).filter(...)`, sorted by provider name with Node's
/// collation (upstream `localeCompare`).
fn candidates<P: Port + VproviderSources>(port: &P) -> Result<Vec<CandidateProvider>, String> {
    let mut providers: Vec<CandidateProvider> = port
        .providers_with_models()
        .into_iter()
        .filter(|provider| !provider.models.is_empty() && !port.is_virtual_provider(&provider.id))
        .collect();
    let collator = rpi_node_collation::collator(&rpi_node_collation::default_locale())?;
    providers.sort_by(|left, right| collator.compare(&left.name, &right.name));
    Ok(providers)
}

/// `pi.registerCommand('vprovider', ...)`: create, edit or remove virtual
/// providers that map one model across provider models.
pub async fn vprovider<U: Ui, P: Port + VproviderSources>(
    ui: &U,
    port: &P,
    args: &str,
) -> Result<(), String> {
    if !ui.has_ui() || port.mode() != "tui" {
        return notify(ui, "/vprovider requires Pi interactive mode.", "warning").await;
    }
    port.reconcile().await?;
    let stored = port.store(json!({"op": "listVirtual"})).await?;
    let stored: Vec<Value> = stored.as_array().cloned().unwrap_or_default();
    let candidates = candidates(port)?;
    let reference = trim(args).to_lowercase();
    let existing = if reference.is_empty() {
        None
    } else {
        stored
            .iter()
            .find(|config| {
                config["id"]
                    .as_str()
                    .map(|id| id.to_lowercase() == reference)
                    .unwrap_or(false)
            })
            .cloned()
    };
    let mut editor = Editor::new(stored.clone(), candidates, existing);
    let available = |id: &str| !port.is_virtual_provider(id) && !port.provider_registered(id);
    let outcome = run_editor(ui, &mut editor, &available).await?;
    match outcome {
        EditorOutcome::Dismissed | EditorOutcome::Discarded => Ok(()),
        EditorOutcome::Removed(id) => {
            let confirmed = confirm(
                ui,
                "Remove virtual provider?",
                &format!("Remove {id}? Backing providers and their pooled accounts are untouched."),
            )
            .await?;
            if !confirmed {
                return Ok(());
            }
            port.store(json!({"op": "removeVirtual", "id": id})).await?;
            port.reconcile().await?;
            notify(ui, &format!("Removed virtual provider \"{id}\"."), "info").await
        }
        EditorOutcome::Saved(draft) => {
            let id = draft["id"].as_str().unwrap_or("").to_owned();
            let verb = if stored
                .iter()
                .any(|config| config["id"].as_str() == Some(id.as_str()))
            {
                "Saved"
            } else {
                "Created"
            };
            port.store(json!({"op": "saveVirtual", "input": draft}))
                .await?;
            port.reconcile().await?;
            let model_id = draft["models"][0]["id"].as_str().unwrap_or("");
            notify(
                ui,
                &format!(
                    "{verb} virtual provider \"{id}\". Select \"{model_id}\" on provider \"{id}\" in /model."
                ),
                "info",
            )
            .await
        }
    }
}
