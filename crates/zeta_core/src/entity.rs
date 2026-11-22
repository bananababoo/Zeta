//! Stable identity of world entities, independent of the engine and of any 1 request.

use serde::{Deserialize, Serialize};

/// Permanent identifier of 1 entity in the world.
///
/// Stored in the Event Log and the database, so it never changes for the life of the
/// entity. Engine handles map to it at the edge of the server, and the AI DM never sees
/// it: AI DM requests use per-request local identifiers instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
#[serde(transparent)]
pub struct EntityId(pub u64);
