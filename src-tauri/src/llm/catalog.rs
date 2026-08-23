use std::sync::LazyLock;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalog {
    pub schema_version: u32,
    pub catalog_version: String,
    pub synchronized_automatically: bool,
    pub pricing_policy: String,
    pub models: Vec<ModelCatalogEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntry {
    pub provider_id: String,
    pub model_id: String,
    pub structured_output: bool,
    pub streaming: bool,
    pub tool_calling: bool,
    pub input_usd_per_million: Option<f64>,
    pub output_usd_per_million: Option<f64>,
}

static CATALOG: LazyLock<ModelCatalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../assets/llm-model-catalog-v1.json"))
        .expect("o catálogo LLM versionado deve conter JSON válido")
});

pub fn catalog() -> &'static ModelCatalog {
    &CATALOG
}

pub fn model_entry(provider_id: &str, model_id: &str) -> Option<&'static ModelCatalogEntry> {
    let normalized_provider = if provider_id == "openai_legacy" {
        "openai"
    } else {
        provider_id
    };
    catalog()
        .models
        .iter()
        .find(|entry| entry.provider_id == normalized_provider && entry.model_id == model_id)
}

pub fn model_price_basis(provider_id: &str, model_id: &str) -> Option<f64> {
    let entry = model_entry(provider_id, model_id)?;
    Some(entry.input_usd_per_million? + entry.output_usd_per_million?)
}

pub fn estimate_cost_usd(
    provider_id: &str,
    model_id: &str,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
) -> Option<f64> {
    let entry = model_entry(provider_id, model_id)?;
    let input_price = entry.input_usd_per_million?;
    let output_price = entry.output_usd_per_million?;
    let input = input_tokens?.max(0) as f64;
    let output = output_tokens?.max(0) as f64;
    Some((input * input_price + output * output_price) / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_versioned_and_never_treats_unknown_price_as_zero() {
        assert_eq!(catalog().schema_version, 2);
        assert!(!catalog().catalog_version.is_empty());
        assert!(!catalog().synchronized_automatically);
        assert_eq!(catalog().pricing_policy, "unknown_is_null");
        assert!(model_entry("openai", "gpt-4.1-mini").is_some());
        assert_eq!(model_price_basis("unknown", "unknown"), None);
        assert_eq!(
            estimate_cost_usd("openai", "gpt-4.1-mini", Some(1), Some(1)),
            None
        );
    }

    #[test]
    fn catalog_exposes_capabilities_without_model_aliases_in_migrations() {
        let entry = model_entry("openai", "gpt-4.1-mini").unwrap();
        assert!(entry.structured_output);
        assert!(entry.streaming);
        assert!(entry.tool_calling);
    }
}
