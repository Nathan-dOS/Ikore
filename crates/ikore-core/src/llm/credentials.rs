use keyring::{Entry, Error as KeyringError};

use super::error::{LlmError, LlmErrorKind};

pub const KEYRING_SERVICE: &str = "ikore.runtime";
/// The service used before the rename to Ikore. A credential found only there is
/// moved to [`KEYRING_SERVICE`] the first time it is read.
pub const LEGACY_KEYRING_SERVICE: &str = "hard-vibe-code.runtime";

pub trait CredentialStore: Send + Sync {
    fn get(&self, credential_ref: &str) -> Result<Option<String>, LlmError>;
    fn set(&self, credential_ref: &str, secret: &str) -> Result<(), LlmError>;
    fn delete(&self, credential_ref: &str) -> Result<bool, LlmError>;
}

#[derive(Debug, Default)]
pub struct KeyringCredentialStore;

/// Raw access to one keyring service. Separated so the move from the legacy
/// service can be tested without the operating system's credential store.
trait SecretBackend {
    fn read(&self, service: &str, credential_ref: &str) -> Result<Option<String>, LlmError>;
    fn write(&self, service: &str, credential_ref: &str, secret: &str) -> Result<(), LlmError>;
    fn remove(&self, service: &str, credential_ref: &str) -> Result<bool, LlmError>;
}

struct OsKeyring;

impl OsKeyring {
    fn entry(service: &str, credential_ref: &str) -> Result<Entry, LlmError> {
        Entry::new(service, credential_ref).map_err(|error| {
            LlmError::new(
                LlmErrorKind::Persistence,
                format!("Failed to access the keyring: {error}"),
            )
        })
    }
}

impl SecretBackend for OsKeyring {
    fn read(&self, service: &str, credential_ref: &str) -> Result<Option<String>, LlmError> {
        match Self::entry(service, credential_ref)?.get_password() {
            Ok(secret) if secret.trim().is_empty() => Ok(None),
            Ok(secret) => Ok(Some(secret.trim().to_string())),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(LlmError::new(
                LlmErrorKind::Persistence,
                format!("Failed to read the credential from the keyring: {error}"),
            )),
        }
    }

    fn write(&self, service: &str, credential_ref: &str, secret: &str) -> Result<(), LlmError> {
        Self::entry(service, credential_ref)?
            .set_password(secret)
            .map_err(|error| {
                LlmError::new(
                    LlmErrorKind::Persistence,
                    format!("Failed to save the credential to the keyring: {error}"),
                )
            })
    }

    fn remove(&self, service: &str, credential_ref: &str) -> Result<bool, LlmError> {
        match Self::entry(service, credential_ref)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(KeyringError::NoEntry) => Ok(false),
            Err(error) => Err(LlmError::new(
                LlmErrorKind::Persistence,
                format!("Failed to remove the credential from the keyring: {error}"),
            )),
        }
    }
}

/// Reads the current service first. A credential that exists only under the
/// legacy service is copied to the current one, read back, and only then removed
/// from the legacy service, so a failure never loses it.
fn read_moving_legacy(
    backend: &dyn SecretBackend,
    credential_ref: &str,
) -> Result<Option<String>, LlmError> {
    if let Some(secret) = backend.read(KEYRING_SERVICE, credential_ref)? {
        return Ok(Some(secret));
    }
    let Some(secret) = backend.read(LEGACY_KEYRING_SERVICE, credential_ref)? else {
        return Ok(None);
    };
    backend.write(KEYRING_SERVICE, credential_ref, &secret)?;
    if backend.read(KEYRING_SERVICE, credential_ref)?.as_deref() == Some(secret.as_str()) {
        let _ = backend.remove(LEGACY_KEYRING_SERVICE, credential_ref);
    }
    Ok(Some(secret))
}

/// Writes under the current service and drops any legacy copy, which would be
/// stale from now on.
fn write_dropping_legacy(
    backend: &dyn SecretBackend,
    credential_ref: &str,
    secret: &str,
) -> Result<(), LlmError> {
    backend.write(KEYRING_SERVICE, credential_ref, secret)?;
    let _ = backend.remove(LEGACY_KEYRING_SERVICE, credential_ref);
    Ok(())
}

/// Removes the credential from both services, so a deleted credential cannot come
/// back from the legacy service on the next read.
fn remove_from_both(backend: &dyn SecretBackend, credential_ref: &str) -> Result<bool, LlmError> {
    let current = backend.remove(KEYRING_SERVICE, credential_ref)?;
    let legacy = backend.remove(LEGACY_KEYRING_SERVICE, credential_ref)?;
    Ok(current || legacy)
}

// Unit tests of this crate build services on this store; none of them may
// reach the operating system's credential store. It reads as empty there and
// refuses writes, so a test that needs a stored secret injects its own store.
impl CredentialStore for KeyringCredentialStore {
    fn get(&self, credential_ref: &str) -> Result<Option<String>, LlmError> {
        if cfg!(test) {
            return Ok(None);
        }
        read_moving_legacy(&OsKeyring, credential_ref)
    }

    fn set(&self, credential_ref: &str, secret: &str) -> Result<(), LlmError> {
        let normalized = secret.trim();
        if normalized.is_empty() {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "apiKey cannot be empty.",
            ));
        }
        if cfg!(test) {
            return Err(LlmError::new(
                LlmErrorKind::Persistence,
                "The keyring is not reachable from unit tests.",
            ));
        }
        write_dropping_legacy(&OsKeyring, credential_ref, normalized)
    }

    fn delete(&self, credential_ref: &str) -> Result<bool, LlmError> {
        if cfg!(test) {
            return Ok(false);
        }
        remove_from_both(&OsKeyring, credential_ref)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct MemoryBackend {
        entries: Mutex<HashMap<(String, String), String>>,
        refuse_writes: bool,
    }

    impl MemoryBackend {
        fn with(service: &str, credential_ref: &str, secret: &str) -> Self {
            let backend = Self::default();
            backend.entries.lock().unwrap().insert(
                (service.to_string(), credential_ref.to_string()),
                secret.to_string(),
            );
            backend
        }

        fn has(&self, service: &str, credential_ref: &str) -> bool {
            self.entries
                .lock()
                .unwrap()
                .contains_key(&(service.to_string(), credential_ref.to_string()))
        }
    }

    impl SecretBackend for MemoryBackend {
        fn read(&self, service: &str, credential_ref: &str) -> Result<Option<String>, LlmError> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .get(&(service.to_string(), credential_ref.to_string()))
                .cloned())
        }

        fn write(&self, service: &str, credential_ref: &str, secret: &str) -> Result<(), LlmError> {
            if self.refuse_writes {
                return Err(LlmError::new(LlmErrorKind::Persistence, "refused"));
            }
            self.entries.lock().unwrap().insert(
                (service.to_string(), credential_ref.to_string()),
                secret.to_string(),
            );
            Ok(())
        }

        fn remove(&self, service: &str, credential_ref: &str) -> Result<bool, LlmError> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .remove(&(service.to_string(), credential_ref.to_string()))
                .is_some())
        }
    }

    #[test]
    fn a_legacy_credential_moves_to_the_current_service_on_first_read() {
        let backend = MemoryBackend::with(LEGACY_KEYRING_SERVICE, "provider:1", "fixture-secret");

        let secret = read_moving_legacy(&backend, "provider:1").unwrap();

        assert_eq!(secret.as_deref(), Some("fixture-secret"));
        assert!(backend.has(KEYRING_SERVICE, "provider:1"));
        assert!(!backend.has(LEGACY_KEYRING_SERVICE, "provider:1"));
    }

    #[test]
    fn a_failed_copy_keeps_the_legacy_credential() {
        let mut backend =
            MemoryBackend::with(LEGACY_KEYRING_SERVICE, "provider:1", "fixture-secret");
        backend.refuse_writes = true;

        assert!(read_moving_legacy(&backend, "provider:1").is_err());
        assert!(backend.has(LEGACY_KEYRING_SERVICE, "provider:1"));
    }

    #[test]
    fn the_current_service_wins_over_a_stale_legacy_copy() {
        let backend = MemoryBackend::with(KEYRING_SERVICE, "provider:1", "current-secret");
        backend
            .write(LEGACY_KEYRING_SERVICE, "provider:1", "stale-secret")
            .unwrap();

        assert_eq!(
            read_moving_legacy(&backend, "provider:1")
                .unwrap()
                .as_deref(),
            Some("current-secret")
        );
    }

    #[test]
    fn writing_and_deleting_also_clear_the_legacy_service() {
        let backend = MemoryBackend::with(LEGACY_KEYRING_SERVICE, "provider:1", "old-secret");
        write_dropping_legacy(&backend, "provider:1", "new-secret").unwrap();
        assert!(!backend.has(LEGACY_KEYRING_SERVICE, "provider:1"));

        backend
            .write(LEGACY_KEYRING_SERVICE, "provider:1", "old-secret")
            .unwrap();
        assert!(remove_from_both(&backend, "provider:1").unwrap());
        assert_eq!(read_moving_legacy(&backend, "provider:1").unwrap(), None);
    }

    #[test]
    fn nothing_stored_reads_as_none() {
        assert_eq!(
            read_moving_legacy(&MemoryBackend::default(), "provider:1").unwrap(),
            None
        );
    }
}
