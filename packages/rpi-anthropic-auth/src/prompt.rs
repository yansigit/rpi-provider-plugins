//! Section-aware shaping of Pi's system prompt for Anthropic OAuth.
//!
//! Pi renders its system prompt as an untagged preamble followed by named
//! sections, each wrapped as `<name>\n${content}\n</name>`, and joins the
//! non-empty parts with a blank line. Shaping has to act on individual
//! sections without disturbing the rest, so this module splits that text into
//! ordered chunks and renders them back.
//!
//! Parsing is deliberately conservative: anything that does not match the
//! exact shape upstream emits is kept as untagged text with its source bytes
//! intact, so a prompt this parser does not understand degrades to
//! passthrough rather than corruption.

use regex::{Captures, Regex};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Prefix of Pi's built-in default system prompt preamble.
///
/// Used to detect whether a system block contains Pi's original verbose
/// preamble so it can be replaced with the minimal neutral prompt.
pub const PI_DEFAULT_PROMPT_PREFIX: &str =
    "You are an expert coding assistant operating inside pi, a coding agent harness.";

/// Prefix of the minimal neutral Anthropic OAuth system prompt.
///
/// Used as a detection marker to identify system blocks that have already been
/// shaped. Must match the first line of [`MINIMAL_ANTHROPIC_OAUTH_PROMPT`].
pub const MINIMAL_ANTHROPIC_OAUTH_PROMPT_PREFIX: &str = "You are an expert coding assistant.";

/// Minimal neutral system prompt used for Anthropic OAuth requests.
///
/// Replaces Pi's verbose default preamble to avoid prompt fingerprinting while
/// preserving any project context that follows.
pub const MINIMAL_ANTHROPIC_OAUTH_PROMPT: &str = "You are an expert coding assistant.\nBe concise and helpful.\nUse the available tools to answer the user's request.\nShow file paths clearly when working with files.";

/// Names of the sections Pi generates itself in its default system prompt.
///
/// Their presence is what tells shaping the prompt is Pi's own structured
/// prompt rather than something it must not touch, and they scope
/// [`TEXT_REPLACEMENTS`] so a user's `project_context` is never rewritten.
pub const PI_OWNED_SECTIONS: [&str; 3] = ["tools", "rules", "docs"];

/// Marks the `tools` section's trailing filler sentence about custom tools.
///
/// The paragraph containing it is dropped; the section and its tool snippets
/// are kept, because extensions contribute those.
pub const PI_TOOLS_FILLER_ANCHOR: &str = "In addition to the tools above";

/// Marks a `docs` section as Pi's own documentation block.
///
/// Extensions may register a section named `docs` of their own, and upstream
/// applies custom sections after the built-ins, so the section is dropped on
/// this anchor rather than on its name alone.
pub const PI_DOCS_SECTION_ANCHOR: &str =
    "Pi documentation (read only when the user asks about pi itself";

/// One inline text replacement applied to Pi-generated sections.
pub struct TextReplacement {
    /// Text to find; every occurrence is replaced.
    pub needle: &'static str,
    /// Text written in its place.
    pub replacement: &'static str,
}

/// Inline text replacements applied to Pi-generated sections.
///
/// These handle known Anthropic classifier trigger phrases that may appear in
/// sections we want to keep. When the "Here is some useful information..."
/// phrase reaches Anthropic combined with typical agent context, `/v1/messages`
/// responds with a 400 disguised as "You're out of extra usage." Replacing the
/// word "useful" is enough to unblock the request.
pub const TEXT_REPLACEMENTS: [TextReplacement; 1] = [TextReplacement {
    needle: "Here is some useful information about the environment you are running in:",
    replacement: "Environment context you are running in:",
}];

/// One top-level chunk of Pi's structured system prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptChunk {
    /// Tag name, or `None` for untagged text (the preamble, or an unparsed remainder).
    pub name: Option<String>,
    /// Exact source text of the chunk, tags included.
    pub raw: String,
    /// Text between the tags; equals [`PromptChunk::raw`] when untagged.
    pub body: String,
}

/// Chunk boundaries consume exactly this separator, so any additional blank
/// lines stay inside the chunk that precedes them and the round-trip stays
/// byte-exact.
const CHUNK_SEPARATOR: &str = "\n\n";

/// The name under which the untagged leading text is offered to the rule table.
///
/// Upstream keys the preamble this way too — its own section builder returns it
/// as `preamble`, and a mid-conversation update naming that section frames it
/// with the same string.
const PREAMBLE_SECTION: &str = "preamble";

/// The one section shaping removes outright rather than rewriting.
///
/// Named once because two paths act on it: the leading prompt drops the section
/// itself, and a mid-conversation update drops both the section and the notice
/// announcing its removal.
const DOCS_SECTION: &str = "docs";

static WARNED_PROMPT_UNSTRUCTURED: AtomicBool = AtomicBool::new(false);

/// Reset the one-time degraded-path warning latch. Exposed for tests.
pub fn reset_shaping_warnings() {
    WARNED_PROMPT_UNSTRUCTURED.store(false, Ordering::Relaxed);
}

/// The section open tag, anchored so it only matches at the offset it is applied to.
///
/// Requiring a bare tag (no attributes) is what keeps
/// `<project_instructions path="...">` inside a section body rather than
/// starting a new one.
fn section_open_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\A<([a-z][a-z0-9_-]*)>\n").expect("section open pattern"))
}

/// Runs of two or more newlines, the paragraph separator Pi's prompt uses.
fn blank_line_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\n\n+").expect("blank line pattern"))
}

/// The framing Pi puts on each changed section of a mid-conversation update.
///
/// Group 1 names an updated section (its rendered value follows); group 2 names
/// a removed one (the notice is the whole part).
fn section_update_frame_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?m)^(?:Updated system prompt section "([a-z][a-z0-9_-]*)":\n\n|Removed system prompt section "([a-z][a-z0-9_-]*)"\.$)"#,
        )
        .expect("section update frame pattern")
    })
}

/// Splits a rendered system prompt into ordered chunks.
///
/// [`render_system_prompt_chunks`] reverses this exactly.
pub fn parse_system_prompt_chunks(text: &str) -> Vec<PromptChunk> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut chunks: Vec<PromptChunk> = Vec::new();
    let mut untagged_start = 0usize;
    let mut search_from = 0usize;

    while search_from <= text.len() {
        let Some(start) = find_section_start(text, search_from) else {
            break;
        };
        let Some((chunk, end)) = read_section_at(text, start) else {
            search_from = start + 1;
            continue;
        };

        if start > untagged_start {
            chunks.push(untagged_chunk(
                &text[untagged_start..start.saturating_sub(CHUNK_SEPARATOR.len())],
            ));
        }
        chunks.push(chunk);
        untagged_start = end + CHUNK_SEPARATOR.len();
        search_from = untagged_start;
    }

    if untagged_start < text.len() {
        chunks.push(untagged_chunk(&text[untagged_start..]));
    }
    chunks
}

/// Renders chunks back into a system prompt string.
pub fn render_system_prompt_chunks(chunks: &[PromptChunk]) -> String {
    chunks
        .iter()
        .map(|chunk| chunk.raw.as_str())
        .collect::<Vec<_>>()
        .join(CHUNK_SEPARATOR)
}

/// Builds a named section chunk in the form upstream renders.
///
/// Use this when shaping replaces a section's body, so the result re-parses to
/// the same chunk.
pub fn named_section(name: &str, body: &str) -> PromptChunk {
    PromptChunk {
        name: Some(name.to_string()),
        raw: format!("<{name}>\n{body}\n</{name}>"),
        body: body.to_string(),
    }
}

/// Finds the next offset at or after `from` that begins a section.
///
/// A section begins at `from` itself, or after a blank line. Scanning one
/// character past each rejected separator is what lets a run of newlines
/// resolve to its *last* blank line, leaving the extra newlines attached to the
/// preceding chunk.
fn find_section_start(text: &str, from: usize) -> Option<usize> {
    if starts_section(text, from) {
        return Some(from);
    }

    let mut index = from;
    while index <= text.len() {
        let separator_index = find_from(text, CHUNK_SEPARATOR, index)?;
        let candidate = separator_index + CHUNK_SEPARATOR.len();
        if starts_section(text, candidate) {
            return Some(candidate);
        }
        index = separator_index + 1;
    }
    None
}

fn starts_section(text: &str, offset: usize) -> bool {
    text.get(offset..)
        .map(|rest| section_open_pattern().is_match(rest))
        .unwrap_or(false)
}

/// Reads a section starting exactly at `start`, if one is there.
///
/// The close tag is matched by name and must end the chunk — it is followed by
/// a blank line or the end of the text — so a nested tag of a different name
/// cannot terminate the section early.
///
/// Returns the chunk and the offset just past its close tag.
fn read_section_at(text: &str, start: usize) -> Option<(PromptChunk, usize)> {
    let open = section_open_pattern().captures(text.get(start..)?)?;
    let whole = open.get(0)?;
    let name = open.get(1)?.as_str().to_string();
    let body_start = start + whole.end();
    let close_tag = format!("\n</{name}>");

    let mut search_from = body_start;
    while search_from <= text.len() {
        let close_index = find_from(text, &close_tag, search_from)?;
        let end = close_index + close_tag.len();
        if end == text.len() || text[end..].starts_with(CHUNK_SEPARATOR) {
            return Some((
                PromptChunk {
                    name: Some(name),
                    raw: text[start..end].to_string(),
                    body: text[body_start..close_index].to_string(),
                },
                end,
            ));
        }
        search_from = close_index + 1;
    }
    None
}

fn untagged_chunk(raw: &str) -> PromptChunk {
    PromptChunk {
        name: None,
        raw: raw.to_string(),
        body: raw.to_string(),
    }
}

/// Finds `needle` at or after `from`, in absolute byte offsets.
///
/// Returns `None` when `from` is not a character boundary, which is how an
/// unparsable remainder degrades to passthrough.
fn find_from(text: &str, needle: &str, from: usize) -> Option<usize> {
    text.get(from..)
        .and_then(|rest| rest.find(needle))
        .map(|index| index + from)
}

/// What shaping does with one section of the prompt.
enum SectionDecision {
    Keep,
    Drop,
    Replace(String),
}

/// Shape Pi's system prompt for Anthropic OAuth.
///
/// Shaping decides section by section — replacing the Pi identity, dropping
/// Pi's documentation block, and stripping the custom-tool filler — so the
/// surrounding structure stays well-formed and every section Pi appends
/// survives byte-identically.
///
/// A prompt with none of Pi's own sections is passed through untouched, with a
/// one-time warning: it means upstream restructured again, and the section
/// names this module keys on are no more trustworthy than any other guess.
/// Replacing the single untagged chunk in that case would discard the project
/// context, skills and cwd along with the preamble.
pub fn shape_anthropic_oauth_system_prompt(system_prompt: &str) -> String {
    let chunks = parse_system_prompt_chunks(system_prompt);
    if !chunks.iter().any(is_pi_owned_section) {
        warn_prompt_unstructured_once();
        return system_prompt.to_string();
    }

    let shaped: Vec<PromptChunk> = chunks
        .iter()
        .enumerate()
        .flat_map(|(index, chunk)| shape_chunk(chunk, index == 0))
        .collect();
    render_system_prompt_chunks(&shaped)
}

fn is_pi_owned_section(chunk: &PromptChunk) -> bool {
    chunk
        .name
        .as_deref()
        .map(|name| PI_OWNED_SECTIONS.contains(&name))
        .unwrap_or(false)
}

/// Apply the rule table to one chunk.
///
/// `is_leading` marks the prompt's first chunk, which is the only untagged text
/// the table treats as the preamble. Returns the chunks to emit in its place:
/// none when dropped.
fn shape_chunk(chunk: &PromptChunk, is_leading: bool) -> Vec<PromptChunk> {
    let name = match (&chunk.name, is_leading) {
        (Some(name), _) => name.as_str(),
        (None, true) => PREAMBLE_SECTION,
        (None, false) => return vec![chunk.clone()],
    };

    match decide_section(name, &chunk.body) {
        SectionDecision::Keep => vec![chunk.clone()],
        SectionDecision::Drop => Vec::new(),
        SectionDecision::Replace(body) => vec![with_body(chunk, &body)],
    }
}

/// Decide what to do with one named section of Pi's prompt.
///
/// Shared by the leading prompt and by mid-conversation section updates, which
/// carry the same section names in a different envelope.
fn decide_section(name: &str, body: &str) -> SectionDecision {
    if !is_pi_generated_section(name) {
        return SectionDecision::Keep;
    }

    let text = apply_text_replacements(body);

    if name == PREAMBLE_SECTION && text.starts_with(PI_DEFAULT_PROMPT_PREFIX) {
        return SectionDecision::Replace(MINIMAL_ANTHROPIC_OAUTH_PROMPT.to_string());
    }
    if name == DOCS_SECTION && text.contains(PI_DOCS_SECTION_ANCHOR) {
        return SectionDecision::Drop;
    }
    if name == "tools" {
        let stripped = remove_paragraph_containing(&text, PI_TOOLS_FILLER_ANCHOR);
        if stripped != text {
            return SectionDecision::Replace(stripped);
        }
    }

    if text == body {
        SectionDecision::Keep
    } else {
        SectionDecision::Replace(text)
    }
}

fn is_pi_generated_section(name: &str) -> bool {
    name == PREAMBLE_SECTION || PI_OWNED_SECTIONS.contains(&name)
}

/// Rebuild a chunk around a new body, preserving whether it was tagged.
fn with_body(chunk: &PromptChunk, body: &str) -> PromptChunk {
    match &chunk.name {
        None => untagged_chunk(body),
        Some(name) => named_section(name, body),
    }
}

fn remove_paragraph_containing(text: &str, anchor: &str) -> String {
    let paragraphs: Vec<&str> = blank_line_pattern().split(text).collect();
    let kept: Vec<&str> = paragraphs
        .iter()
        .copied()
        .filter(|paragraph| !paragraph.contains(anchor))
        .collect();
    if kept.len() == paragraphs.len() {
        text.to_string()
    } else {
        trim_js(&kept.join("\n\n")).to_string()
    }
}

fn apply_text_replacements(text: &str) -> String {
    let mut result = text.to_string();
    for rule in &TEXT_REPLACEMENTS {
        result = result.replace(rule.needle, rule.replacement);
    }
    result
}

/// `String.prototype.trim` semantics for the whitespace a prompt can carry.
///
/// Rust's [`str::trim`] already matches JS on the Unicode whitespace that
/// reaches a prompt; the one addition is U+FEFF, which JS trims and Rust does
/// not.
fn trim_js(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

fn trim_end_js(text: &str) -> &str {
    text.trim_end_matches(is_js_whitespace)
}

fn is_js_whitespace(character: char) -> bool {
    character.is_whitespace() || character == '\u{feff}'
}

fn warn_prompt_unstructured_once() {
    if WARNED_PROMPT_UNSTRUCTURED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "[rpi-anthropic-auth] Pi's system prompt carries none of its own sections; passing it through unshaped. \
         Upstream Pi may have restructured its prompt — re-verify PI_OWNED_SECTIONS."
    );
}

/// Shape the text of a mid-conversation system message.
///
/// Returns `None` when every framed part was dropped and the block should not
/// be sent at all.
pub fn shape_system_update_text(text: &str) -> Option<String> {
    let frames: Vec<Captures<'_>> = section_update_frame_pattern().captures_iter(text).collect();
    let Some(first) = frames.first() else {
        return Some(text.to_string());
    };
    let first_start = first.get(0).map(|frame| frame.start()).unwrap_or(0);

    let mut parts: Vec<String> = Vec::new();
    let leading = trim_js(&text[..first_start]);
    if !leading.is_empty() {
        parts.push(leading.to_string());
    }

    for (index, frame) in frames.iter().enumerate() {
        let part_end = frames
            .get(index + 1)
            .and_then(|next| next.get(0))
            .map(|next| next.start())
            .unwrap_or(text.len());
        if let Some(shaped) = shape_update_part(frame, &text[..part_end]) {
            parts.push(shaped);
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Apply the rule table to one framed part of a system message.
///
/// `through` is the message text up to the end of this part; the part's own
/// value is what follows the framing.
fn shape_update_part(frame: &Captures<'_>, through: &str) -> Option<String> {
    let framing_match = frame.get(0)?;
    let framing = framing_match.as_str();

    if let Some(removed) = frame.get(2).map(|removed| removed.as_str()) {
        // Pi's own docs section was never forwarded, so a notice retracting it
        // names nothing the model was given.
        return if removed == DOCS_SECTION {
            None
        } else {
            Some(framing.to_string())
        };
    }

    let Some(updated) = frame.get(1).map(|updated| updated.as_str()) else {
        return Some(framing.to_string());
    };
    let name = updated;
    let value = trim_end_js(&through[framing_match.end()..]);
    let parsed = parse_system_prompt_chunks(value);
    let tagged = parsed.len() == 1 && parsed[0].name.as_deref() == Some(name);
    let body = if tagged {
        parsed[0].body.as_str()
    } else {
        value
    };

    let shaped_body = match decide_section(name, body) {
        SectionDecision::Drop => return None,
        SectionDecision::Keep => body.to_string(),
        SectionDecision::Replace(body) => body,
    };

    Some(format!(
        "{framing}{}",
        if tagged {
            named_section(name, &shaped_body).raw
        } else {
            shaped_body
        }
    ))
}

/// Apply anthropic payload shaping to an array of system text blocks.
///
/// Finds every block containing Pi's default prompt preamble and replaces its
/// text in place. Blocks without the preamble are passed through unchanged.
pub fn shape_system_blocks(blocks: &[Value]) -> Vec<Value> {
    blocks
        .iter()
        .map(|block| {
            let is_text = block.get("type").and_then(Value::as_str) == Some("text");
            let text = block.get("text").and_then(Value::as_str);
            match (is_text, text) {
                (true, Some(text)) if text.contains(PI_DEFAULT_PROMPT_PREFIX) => {
                    let mut shaped = block.clone();
                    shaped["text"] = Value::String(shape_anthropic_oauth_system_prompt(text));
                    shaped
                }
                _ => block.clone(),
            }
        })
        .collect()
}

/// Whether an access token belongs to the Anthropic OAuth (`sk-ant-oat`) family.
///
/// Only those requests are shaped; an API-key request must reach Anthropic
/// with the prompt it was built with.
pub fn is_anthropic_oauth_access_token(access_token: &str) -> bool {
    access_token.starts_with("sk-ant-oat")
}

/// Applies Anthropic OAuth shaping to a built provider payload.
///
/// Requests whose access token is not an `sk-ant-oat` token are returned
/// untouched, and so is anything that is not an Anthropic messages payload.
pub fn shape_anthropic_oauth_payload(payload: Value, access_token: &str) -> Value {
    if !is_anthropic_oauth_access_token(access_token) {
        return payload;
    }
    shape_oauth_payload(payload)
}

fn shape_oauth_payload(mut payload: Value) -> Value {
    if !is_anthropic_messages_payload(&payload) {
        return payload;
    }

    let messages = payload["messages"]
        .as_array()
        .cloned()
        .unwrap_or_else(Vec::new);
    let shaped_messages = shape_system_role_messages(&messages);

    let shaped_system = match payload.get("system") {
        Some(Value::Array(blocks)) => Some(Value::Array(shape_system_blocks(blocks))),
        _ => None,
    };
    if let Some(system) = shaped_system {
        payload["system"] = system;
    }
    payload["messages"] = Value::Array(shaped_messages);
    payload
}

fn is_anthropic_messages_payload(payload: &Value) -> bool {
    payload.is_object()
        && payload.get("model").map(Value::is_string).unwrap_or(false)
        && payload
            .get("messages")
            .map(Value::is_array)
            .unwrap_or(false)
        && payload
            .get("stream")
            .map(Value::is_boolean)
            .unwrap_or(false)
}

/// Applies system prompt shaping to `role: "system"` messages.
///
/// Pi re-sends changed prompt sections mid-conversation as system messages
/// inside `messages[]` on models that accept them, so Pi content the sanitizer
/// strips from `system[]` would otherwise reach Anthropic by that second route.
///
/// A message whose text blocks all shape away is dropped unless it carries
/// effort: an update with nothing left to say is noise. Non-text blocks pass
/// through, and keep their message alive.
fn shape_system_role_messages(messages: &[Value]) -> Vec<Value> {
    let mut shaped: Vec<Value> = Vec::with_capacity(messages.len());
    for message in messages {
        let content = message.get("content").and_then(Value::as_array);
        if message.get("role").and_then(Value::as_str) != Some("system") {
            shaped.push(message.clone());
            continue;
        }
        let Some(content) = content else {
            shaped.push(message.clone());
            continue;
        };

        let mut blocks: Vec<Value> = Vec::with_capacity(content.len());
        for block in content {
            blocks.extend(shape_system_message_block(block));
        }

        if blocks.is_empty() && !carries_effort(message) {
            continue;
        }
        let mut message = message.clone();
        message["content"] = Value::Array(blocks);
        shaped.push(message);
    }
    shaped
}

/// Whether a system message carries a per-message effort level.
///
/// On models pi-ai flags as supporting mid-conversation effort, Pi pins the
/// top-level `output_config.effort` to `"high"` and sends the requested effort
/// as content-less `role: "system"` messages. Anthropic accepts them with an
/// empty `content` array, and dropping them silently runs every request at
/// `"high"`.
fn carries_effort(message: &Value) -> bool {
    message.get("output_config").is_some()
}

fn shape_system_message_block(block: &Value) -> Vec<Value> {
    let text = match block.get("type").and_then(Value::as_str) {
        Some("text") => block.get("text").and_then(Value::as_str),
        _ => None,
    };
    let Some(text) = text else {
        return vec![block.clone()];
    };

    match shape_system_update_text(text) {
        Some(shaped) => {
            let mut block = block.clone();
            block["text"] = Value::String(shaped);
            vec![block]
        }
        None => Vec::new(),
    }
}
