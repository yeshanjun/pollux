use super::scheduler::CredentialId;
use crate::db::{CodexCreate, CodexPatch, DbActorHandle, ProviderCreate, ProviderPatch};
use crate::error::PolluxError;
use crate::providers::codex::resource::CodexResource;

#[derive(Clone)]
pub struct CredentialOps {
    db: DbActorHandle,
}

impl CredentialOps {
    pub fn new(db: DbActorHandle) -> Self {
        Self { db }
    }

    pub async fn load_active(&self) -> Result<Vec<(CredentialId, CodexResource)>, PolluxError> {
        let rows = self.db.list_active_codex().await?;
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let id = u64::try_from(row.id).map_err(|_| {
                PolluxError::UnexpectedError(format!("Invalid credential id {}", row.id))
            })?;
            result.push((id, row.into()));
        }
        Ok(result)
    }

    pub async fn upsert(&self, cred: CodexResource) -> Result<CredentialId, PolluxError> {
        let create: CodexCreate = cred.into();
        let id = self.db.create(ProviderCreate::Codex(create)).await?;

        u64::try_from(id)
            .map_err(|_| PolluxError::UnexpectedError(format!("Invalid credential id {}", id)))
    }

    pub async fn update_by_id(
        &self,
        id: CredentialId,
        patch: CodexPatch,
    ) -> Result<(), PolluxError> {
        let _ = i64::try_from(id)
            .map_err(|_| PolluxError::UnexpectedError(format!("Invalid credential id {}", id)))?;

        self.db.patch(ProviderPatch::Codex { id, patch }).await?;
        Ok(())
    }

    pub async fn set_status(&self, id: CredentialId, status: bool) -> Result<(), PolluxError> {
        let _ = i64::try_from(id)
            .map_err(|_| PolluxError::UnexpectedError(format!("Invalid credential id {}", id)))?;
        let patch = CodexPatch {
            status: Some(status),
            ..Default::default()
        };
        self.db.patch(ProviderPatch::Codex { id, patch }).await
    }
}
