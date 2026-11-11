//! Environment variable resolution with backward compatibility.
//!
//! Canonical variables use the `IKORE_` prefix. When unset, they fall back
//! to legacy `HVC_` equivalents and log a warning to stderr.

use std::ffi::OsString;

/// Reads an environment variable as an [`OsString`], checking `IKORE_<SUFFIX>` first,
/// then falling back to `HVC_<SUFFIX>` with a deprecation notice.
pub fn var_os_with_legacy(suffix: &str) -> Option<OsString> {
    let canonical = format!("IKORE_{suffix}");
    if let Some(val) = std::env::var_os(&canonical) {
        return Some(val);
    }
    let legacy = format!("HVC_{suffix}");
    if let Some(val) = std::env::var_os(&legacy) {
        eprintln!("Warning: {legacy} is deprecated; use {canonical} instead.");
        return Some(val);
    }
    None
}

/// Reads an environment variable as a [`String`], checking `IKORE_<SUFFIX>` first,
/// then falling back to `HVC_<SUFFIX>` with a deprecation notice.
pub fn var_with_legacy(suffix: &str) -> Result<String, std::env::VarError> {
    let canonical = format!("IKORE_{suffix}");
    match std::env::var(&canonical) {
        Ok(val) => Ok(val),
        Err(std::env::VarError::NotPresent) => {
            let legacy = format!("HVC_{suffix}");
            match std::env::var(&legacy) {
                Ok(val) => {
                    eprintln!("Warning: {legacy} is deprecated; use {canonical} instead.");
                    Ok(val)
                }
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

/// Checks whether a boolean environment flag is enabled (`1`, `true`, `yes`, `on`),
/// checking `IKORE_<SUFFIX>` first, then `HVC_<SUFFIX>`.
pub fn env_flag_with_legacy(suffix: &str) -> bool {
    var_with_legacy(suffix).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_takes_precedence_over_legacy() {
        // SAFETY: Test-local variables modified sequentially in this single-threaded test.
        unsafe {
            std::env::set_var("IKORE_TEST_FLAG_XYZ", "1");
            std::env::set_var("HVC_TEST_FLAG_XYZ", "0");
        }
        assert!(env_flag_with_legacy("TEST_FLAG_XYZ"));
        assert_eq!(var_with_legacy("TEST_FLAG_XYZ").unwrap(), "1");
        // SAFETY: Test cleanup of test-local variables.
        unsafe {
            std::env::remove_var("IKORE_TEST_FLAG_XYZ");
            std::env::remove_var("HVC_TEST_FLAG_XYZ");
        }
    }

    #[test]
    fn legacy_falls_back_when_canonical_missing() {
        // SAFETY: Test-local variables modified sequentially in this single-threaded test.
        unsafe {
            std::env::remove_var("IKORE_TEST_FLAG_ABC");
            std::env::set_var("HVC_TEST_FLAG_ABC", "true");
        }
        assert!(env_flag_with_legacy("TEST_FLAG_ABC"));
        // SAFETY: Test cleanup of test-local variables.
        unsafe {
            std::env::remove_var("HVC_TEST_FLAG_ABC");
        }
    }
}
