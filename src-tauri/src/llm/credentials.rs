use keyring::{Entry, Error as KeyringError};

use super::error::{LlmError, LlmErrorKind};

pub const KEYRING_SERVICE: &str = "hard-vibe-code.runtime";

pub trait CredentialStore: Send + Sync {
    fn get(&self, credential_ref: &str) -> Result<Option<String>, LlmError>;
    fn set(&self, credential_ref: &str, secret: &str) -> Result<(), LlmError>;
    fn delete(&self, credential_ref: &str) -> Result<bool, LlmError>;
}

#[derive(Debug, Default)]
pub struct KeyringCredentialStore;

impl KeyringCredentialStore {
    fn entry(credential_ref: &str) -> Result<Entry, LlmError> {
        Entry::new(KEYRING_SERVICE, credential_ref).map_err(|error| {
            LlmError::new(
                LlmErrorKind::Persistence,
                format!("Falha ao acessar keyring: {error}"),
            )
        })
    }
}

impl CredentialStore for KeyringCredentialStore {
    fn get(&self, credential_ref: &str) -> Result<Option<String>, LlmError> {
        let entry = Self::entry(credential_ref)?;
        match entry.get_password() {
            Ok(secret) if secret.trim().is_empty() => Ok(None),
            Ok(secret) => Ok(Some(secret.trim().to_string())),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(LlmError::new(
                LlmErrorKind::Persistence,
                format!("Falha ao ler credencial do keyring: {error}"),
            )),
        }
    }

    fn set(&self, credential_ref: &str, secret: &str) -> Result<(), LlmError> {
        let normalized = secret.trim();
        if normalized.is_empty() {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "apiKey não pode ser vazia.",
            ));
        }
        Self::entry(credential_ref)?
            .set_password(normalized)
            .map_err(|error| {
                LlmError::new(
                    LlmErrorKind::Persistence,
                    format!("Falha ao salvar credencial no keyring: {error}"),
                )
            })
    }

    fn delete(&self, credential_ref: &str) -> Result<bool, LlmError> {
        match Self::entry(credential_ref)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(KeyringError::NoEntry) => Ok(false),
            Err(error) => Err(LlmError::new(
                LlmErrorKind::Persistence,
                format!("Falha ao remover credencial do keyring: {error}"),
            )),
        }
    }
}
