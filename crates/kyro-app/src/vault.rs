//! Operator-supplied secrets are separate from records, source code and API responses.
use crate::{AppError, AppResult, AppTx};
use base64::Engine;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretBinding {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub adapter_id: Uuid,
    pub reference_id: Uuid,
    pub purposes: BTreeSet<String>,
    pub secret_base64: String,
}
struct Entry {
    adapter_id: Uuid,
    purposes: BTreeSet<String>,
    bytes: Zeroizing<Vec<u8>>,
}
#[derive(Default)]
pub struct SecretVault {
    entries: BTreeMap<(Uuid, Uuid, Uuid), Entry>,
}
impl SecretVault {
    pub fn new(bindings: Vec<SecretBinding>) -> AppResult<Self> {
        if bindings.len() > 1024 {
            return Err(AppError::invalid("vault_entry_limit"));
        }
        let mut entries = BTreeMap::new();
        for binding in bindings {
            let encoded = Zeroizing::new(binding.secret_base64);
            let bytes = Zeroizing::new(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded.as_bytes())
                    .map_err(|_| AppError::invalid("invalid_vault_encoding"))?,
            );
            if bytes.len() < 32
                || bytes.len() > 4096
                || binding.purposes.is_empty()
                || binding.purposes.len() > 16
            {
                return Err(AppError::invalid("invalid_vault_binding"));
            }
            if entries
                .insert(
                    (
                        binding.tenant_id,
                        binding.application_id,
                        binding.reference_id,
                    ),
                    Entry {
                        adapter_id: binding.adapter_id,
                        purposes: binding.purposes,
                        bytes,
                    },
                )
                .is_some()
            {
                return Err(AppError::invalid("duplicate_vault_reference"));
            }
        }
        Ok(Self { entries })
    }
    pub async fn load(path: &Path) -> AppResult<Self> {
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|_| AppError::Unavailable)?;
        if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
            return Err(AppError::invalid("invalid_vault_file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(AppError::invalid("vault_file_permissions"));
            }
        }
        let content = Zeroizing::new(
            tokio::fs::read(path)
                .await
                .map_err(|_| AppError::Unavailable)?,
        );
        let bindings = serde_json::from_slice(&content)
            .map_err(|_| AppError::invalid("invalid_vault_file"))?;
        Self::new(bindings)
    }
    pub(crate) async fn resolve(
        &self,
        tx: &mut AppTx,
        adapter: Uuid,
        reference: Uuid,
        purpose: &str,
    ) -> AppResult<Zeroizing<Vec<u8>>> {
        let binding = tx.get("secret_ref", reference).await?;
        if binding.data["revoked"] != false
            || binding.data["adapter_id"] != serde_json::json!(adapter)
            || !binding.data["purposes"]
                .as_array()
                .is_some_and(|purposes| purposes.iter().any(|p| p == purpose))
        {
            return Err(AppError::Forbidden);
        }
        let reference = Uuid::parse_str(
            binding.data["vault_reference"]
                .as_str()
                .ok_or(AppError::Internal)?,
        )
        .map_err(|_| AppError::Internal)?;
        self.operator_secret(
            tx.actor().tenant_id(),
            tx.actor().application_id(),
            adapter,
            reference,
            purpose,
        )
    }
    pub(crate) fn operator_secret(
        &self,
        tenant: Uuid,
        application: Uuid,
        adapter: Uuid,
        reference: Uuid,
        purpose: &str,
    ) -> AppResult<Zeroizing<Vec<u8>>> {
        let entry = self
            .entries
            .get(&(tenant, application, reference))
            .ok_or(AppError::Unavailable)?;
        if entry.adapter_id != adapter || !entry.purposes.contains(purpose) {
            return Err(AppError::Forbidden);
        }
        Ok(Zeroizing::new(entry.bytes.to_vec()))
    }
}
