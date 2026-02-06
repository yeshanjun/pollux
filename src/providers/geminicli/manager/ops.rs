use super::scheduler::CredentialId;
use crate::db::{DbActorHandle, GeminiCliCreate, GeminiCliPatch, ProviderCreate, ProviderPatch};
use crate::error::PolluxError;
use crate::providers::geminicli::resource::GeminiCliResource;

#[derive(Clone)]
pub struct CredentialOps {
    db: DbActorHandle,
}

impl CredentialOps {
    pub fn new(db: DbActorHandle) -> Self {
        Self { db }
    }

    pub async fn load_active(&self) -> Result<Vec<(CredentialId, GeminiCliResource)>, PolluxError> {
        let rows = self.db.list_active_geminicli().await?;
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let id = u64::try_from(row.id).map_err(|_| {
                PolluxError::UnexpectedError(format!("Invalid credential id {}", row.id))
            })?;
            result.push((id, row.into()));
        }
        Ok(result)
    }

    pub async fn upsert(&self, cred: GeminiCliResource) -> Result<CredentialId, PolluxError> {
        if cred.sub().is_empty() {
            return Err(PolluxError::UnexpectedError(
                "GeminiCli credential missing sub (id_token claims)".to_string(),
            ));
        }
        let create: GeminiCliCreate = cred.into();
        let id = self.db.create(ProviderCreate::GeminiCli(create)).await?;

        u64::try_from(id)
            .map_err(|_| PolluxError::UnexpectedError(format!("Invalid credential id {}", id)))
    }

    pub async fn update_by_id(
        &self,
        id: CredentialId,
        patch: GeminiCliPatch,
    ) -> Result<(), PolluxError> {
        // Keep the same validation semantics: the DB layer uses `i64` ids.
        let _ = i64::try_from(id)
            .map_err(|_| PolluxError::UnexpectedError(format!("Invalid credential id {}", id)))?;

        self.db
            .patch(ProviderPatch::GeminiCli { id, patch })
            .await?;
        Ok(())
    }

    pub async fn set_status(&self, id: CredentialId, status: bool) -> Result<(), PolluxError> {
        // Keep the same validation semantics: the DB layer uses `i64` ids.
        let _ = i64::try_from(id)
            .map_err(|_| PolluxError::UnexpectedError(format!("Invalid credential id {}", id)))?;
        let patch = GeminiCliPatch {
            status: Some(status),
            ..Default::default()
        };
        self.db.patch(ProviderPatch::GeminiCli { id, patch }).await
    }
}
