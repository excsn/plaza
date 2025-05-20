// plaza::app_common::ordered_collection_ops::op_payloads.rs
use crate::agent::AgentId; // If ops need to reference who made the change explicitly
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::hash::Hash; // For CollectionKey and ItemId

/// Payload for an Op to insert an item into an ordered collection.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    CollectionKey: Serialize + for<'de2> Deserialize<'de2>,
    ItemId: Serialize + for<'de2> Deserialize<'de2>,
    ItemPayload: Serialize + for<'de2> Deserialize<'de2>
")]
pub struct InsertListItemPayload<CollectionKey, ItemId, ItemPayload>
where
    CollectionKey: Clone + Debug + Eq + Hash,
    ItemId: Clone + Debug + Eq + Hash,
    ItemPayload: Clone + Debug,
{
    pub collection_key: CollectionKey,
    pub item_id: ItemId, // Newly generated ID for the item being inserted
    pub item_payload: ItemPayload,
    /// Optional: Insert after this existing item_id. If None, usually appends to end or prepends to start.
    pub after_item_id: Option<ItemId>,
    /// Optional: Insert at a specific index. If both index and after_item_id are present,
    /// application defines precedence or how they interact. Using one is often clearer.
    pub at_index: Option<usize>,
}

/// Payload for an Op to remove an item from an ordered collection.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    CollectionKey: Serialize + for<'de2> Deserialize<'de2>,
    ItemId: Serialize + for<'de2> Deserialize<'de2>
")]
pub struct RemoveListItemPayload<CollectionKey, ItemId>
where
    CollectionKey: Clone + Debug + Eq + Hash,
    ItemId: Clone + Debug + Eq + Hash,
{
    pub collection_key: CollectionKey,
    pub item_id_to_remove: ItemId, // Identify item by its unique ID
}

/// Payload for an Op to update the data of an existing item in an ordered collection.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    CollectionKey: Serialize + for<'de2> Deserialize<'de2>,
    ItemId: Serialize + for<'de2> Deserialize<'de2>,
    ItemPayload: Serialize + for<'de2> Deserialize<'de2>
")]
pub struct UpdateListItemPayload<CollectionKey, ItemId, ItemPayload>
where
    CollectionKey: Clone + Debug + Eq + Hash,
    ItemId: Clone + Debug + Eq + Hash,
    ItemPayload: Clone + Debug,
{
    pub collection_key: CollectionKey,
    pub item_id_to_update: ItemId,
    pub new_item_payload: ItemPayload, // Could also be a partial update payload
}

/// Payload for an Op to move/reorder an item within an ordered collection.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    CollectionKey: Serialize + for<'de2> Deserialize<'de2>,
    ItemId: Serialize + for<'de2> Deserialize<'de2>
")]
pub struct MoveListItemPayload<CollectionKey, ItemId>
where
    CollectionKey: Clone + Debug + Eq + Hash,
    ItemId: Clone + Debug + Eq + Hash,
{
    pub collection_key: CollectionKey,
    pub item_id_to_move: ItemId,
    /// Optional: Move item to be after this existing item_id.
    pub new_after_item_id: Option<ItemId>,
    /// Optional: Move item to a specific new index.
    /// Application defines precedence if both are provided.
    pub new_index: Option<usize>,
}

// Notice/Event Payloads (Server -> Client)
// These would mirror the structure of the request payloads but might include more context,
// like the agent_id who performed the action, if not already part of the TargetedOp.
// For example: ItemWasInsertedPayload, ItemWasRemovedPayload, etc.
// For simplicity, StateLogic might just broadcast the state change or the original Op (if idempotent).