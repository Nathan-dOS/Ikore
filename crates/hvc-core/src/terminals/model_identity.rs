#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelVerification {
    Verified,
    Mismatch,
    Unverified,
}

pub(crate) fn verify_model(
    provider: Option<&str>,
    requested: Option<&str>,
    observed: Option<&str>,
) -> ModelVerification {
    let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return ModelVerification::Unverified;
    };
    let Some(observed) = observed.map(str::trim).filter(|value| !value.is_empty()) else {
        return ModelVerification::Unverified;
    };
    let requested = requested.to_ascii_lowercase();
    let observed = observed.to_ascii_lowercase();
    if is_dynamic_alias(&requested) || is_dynamic_alias(&observed) {
        return ModelVerification::Unverified;
    }

    let provider = provider.unwrap_or_default().trim().to_ascii_lowercase();
    // Model IDs from Claude use exact identity unless the requested value is
    // the explicit family alias. A matching family token does not make two
    // dated/versioned Sonnet releases interchangeable.
    let is_claude = matches!(provider.as_str(), "claude" | "anthropic")
        || requested.starts_with("claude-")
        || observed.starts_with("claude-");
    if is_claude && is_claude_family_alias(&requested) {
        return match (claude_family(&requested), claude_family(&observed)) {
            (Some(expected), Some(actual)) if expected == actual => ModelVerification::Verified,
            _ => ModelVerification::Mismatch,
        };
    }

    exact_provider_model_match(&provider, &requested, &observed)
}

fn is_dynamic_alias(model: &str) -> bool {
    matches!(
        model,
        "default" | "auto" | "latest" | "inherit" | "best" | "opusplan" | ""
    ) || model.ends_with("-latest")
}

fn is_claude_family_alias(model: &str) -> bool {
    matches!(
        model.strip_prefix("claude-").unwrap_or(model),
        "haiku" | "sonnet" | "opus"
    )
}

fn claude_family(model: &str) -> Option<&'static str> {
    let normalized = model.strip_prefix("claude-").unwrap_or(model);
    // Claude model IDs have both `claude-haiku-*` and `claude-3-5-haiku-*`
    // forms. Match the named family token, never a substring like `sonnet`.
    let family = normalized
        .split(['-', ':'])
        .find(|token| matches!(*token, "haiku" | "sonnet" | "opus"))?;
    Some(match family {
        "haiku" => "haiku",
        "sonnet" => "sonnet",
        "opus" => "opus",
        _ => return None,
    })
}

fn exact_provider_model_match(
    provider: &str,
    requested: &str,
    observed: &str,
) -> ModelVerification {
    let expected = normalize_model_id(provider, requested);
    let actual = normalize_model_id(provider, observed);
    if expected == actual {
        ModelVerification::Verified
    } else {
        ModelVerification::Mismatch
    }
}

fn normalize_model_id(provider: &str, model: &str) -> String {
    let mut normalized = model.trim().to_ascii_lowercase();
    if provider == "openrouter" || normalized.starts_with("openrouter/") {
        if let Some(value) = normalized.strip_prefix("openrouter/") {
            normalized = value.to_owned();
        }
        if let Some(name) = normalized.strip_suffix(":free") {
            normalized = name.to_owned();
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_aliases_match_only_their_exact_model_family() {
        assert_eq!(
            verify_model(
                Some("claude"),
                Some("haiku"),
                Some("claude-haiku-4-5-20251001")
            ),
            ModelVerification::Verified
        );
        assert_eq!(
            verify_model(Some("claude"), Some("haiku"), Some("claude-sonnet-5")),
            ModelVerification::Mismatch
        );
        assert_eq!(
            verify_model(Some("anthropic"), Some("sonnet"), Some("claude-sonnet-5")),
            ModelVerification::Verified
        );
        assert_eq!(
            verify_model(
                Some("anthropic"),
                Some("claude-sonnet-4-5-20250929"),
                Some("claude-sonnet-5")
            ),
            ModelVerification::Mismatch
        );
    }

    #[test]
    fn provider_model_variants_are_normalized_without_fuzzy_substrings() {
        assert_eq!(
            verify_model(
                Some("openrouter"),
                Some("openrouter/nex-agi/nex-n2.5-pro:free"),
                Some("nex-agi/nex-n2.5-pro")
            ),
            ModelVerification::Verified
        );
        assert_eq!(
            verify_model(Some("openai"), Some("gpt-5.6"), Some("gpt-5.6-luna")),
            ModelVerification::Mismatch
        );
    }

    #[test]
    fn missing_or_dynamic_model_identity_is_unverified() {
        assert_eq!(
            verify_model(Some("claude"), Some("haiku"), None),
            ModelVerification::Unverified
        );
        assert_eq!(
            verify_model(
                Some("claude"),
                Some("default"),
                Some("claude-haiku-4-5-20251001")
            ),
            ModelVerification::Unverified
        );
        for alias in ["inherit", "best", "opusplan"] {
            assert_eq!(
                verify_model(Some("claude"), Some(alias), Some(alias)),
                ModelVerification::Unverified
            );
        }
    }
}
