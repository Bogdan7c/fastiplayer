//! Versioned persistence boundary для canonical playlist state.
//!
//! Crate намеренно отделяет private disk DTO от `playlist-core` domain API.
//! Read-only inspection никогда не переименовывает источник; destructive
//! quarantine выполняется только отдельным policy-вызовом с matching identity.

mod dto;
mod envelope;
mod identity;
mod quarantine;
mod store;
mod types;

#[cfg(test)]
mod tests;

pub use quarantine::QuarantineFileName;
pub use store::PlaylistStateStore;
pub use types::{
    CorruptStateCause, InspectedFileIdentity, InspectionOutcome, LoadedPlaylistState,
    PlaylistStateSnapshot, ProtectedStateCause, QuarantineFailureCause, QuarantineOutcome,
    StateSerializationError,
};

/// Стабильное имя playlist state рядом с application config.
pub const PLAYLIST_STATE_FILENAME: &str = "playlist-state.json";

/// Текущая и единственная поддерживаемая disk schema.
pub const CURRENT_PLAYLIST_STATE_SCHEMA_VERSION: u64 = 1;

/// Hard budget полного envelope proof.
///
/// Он намеренно больше supported-v1 DTO budget: это позволяет распознать и
/// защитить newer schema, не применяя ограничения старого payload.
pub const MAX_STATE_ENVELOPE_SCAN_BYTES: u64 = 40 * 1024 * 1024;

/// Максимум одного decoded JSON string/key token в envelope pass.
pub const MAX_STATE_ENVELOPE_TOKEN_BYTES: usize = 1024 * 1024;

/// Явный depth budget; serde_json recursion limit дополнительно остаётся включён.
pub const MAX_STATE_ENVELOPE_NESTING_DEPTH: usize = 128;

/// Меньший предел файла, который schema v1 разрешает полностью materialize-ить.
pub const MAX_SUPPORTED_V1_STATE_BYTES: u64 = 32 * 1024 * 1024;

/// Сериализует immutable domain snapshot в deterministic pretty JSON.
///
/// Raw URL извлекается только внутри explicit persistence mapping через
/// `SecretUrlLocator::expose_secret_for_persistence`.
pub fn serialize_state(
    snapshot: PlaylistStateSnapshot<'_>,
) -> Result<Vec<u8>, StateSerializationError> {
    dto::serialize_state(snapshot)
}
