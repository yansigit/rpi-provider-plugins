//! Native locale ordering using the pinned Node 24 reference data.
pub use icu_collator::Collator;
use icu_provider_adapters::fallback::LocaleFallbackProvider;
use icu_provider_blob::BlobDataProvider;

pub const DATA: &[u8] = include_bytes!("../data/icu78-collation.postcard");

pub fn collator(locale: &str) -> Result<Collator, String> {
    let locale = locale.parse().map_err(|error| format!("{error}"))?;
    let blob =
        BlobDataProvider::try_new_from_static_blob(DATA).map_err(|error| error.to_string())?;
    let provider = LocaleFallbackProvider::try_new_with_buffer_provider(blob)
        .map_err(|error| error.to_string())?;
    Collator::try_new_with_buffer_provider(&provider, &locale, Default::default())
        .map_err(|error| error.to_string())
}

pub fn default_locale() -> String {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "en-US".into());
    let locale = locale.split(['.', '@']).next().unwrap().replace('_', "-");
    if matches!(locale.as_str(), "C" | "POSIX") {
        "en-US".into()
    } else {
        locale
    }
}
