//! Chunk backends. A backend stores opaque, already-encrypted byte chunks and
//! returns a locator string that is persisted in the `chunks` table.

pub mod local;
pub mod telegram;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

#[async_trait]
pub trait ChunkBackend: Send + Sync {
    fn kind(&self) -> &'static str;
    /// `name` is an opaque random file name; never the user's object key.
    async fn put(&self, name: &str, data: Bytes) -> Result<String>;
    async fn get(&self, locator: &str) -> Result<Bytes>;
    async fn delete(&self, locator: &str) -> Result<()>;
    /// Mark a metadata backup so it can be found without the database (Telegram pins it).
    async fn mark_backup(&self, _locator: &str) -> Result<()> {
        Ok(())
    }
    /// Locator of the newest marked backup, if the backend can discover it.
    async fn latest_backup(&self) -> Result<Option<String>> {
        Ok(None)
    }
    async fn health(&self) -> serde_json::Value;
}
