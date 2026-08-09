//! Optional, composable data structures for common presence information.
//! Applications can embed these within their custom presence detail payloads.

use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::hash::Hash;

/// Represents a 2D cursor position.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct CursorPositionPayload {
  pub x: f32,
  pub y: f32,
  /// Optional: Identifier for the screen, document, or context this cursor position refers to.
  pub context_id: Option<u32>,
}

/// Represents a set of selected item IDs.
/// `ItemID` is generic and defined by the application.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "ItemID: Serialize + for<'de2> Deserialize<'de2>")]
pub struct SelectionPayload<ItemID: Clone + Debug + Eq + Hash> {
  pub selected_item_ids: Vec<ItemID>,
}

impl<ItemID: Clone + Debug + Eq + Hash> Default for SelectionPayload<ItemID> {
  fn default() -> Self {
    Self {
      selected_item_ids: Vec::new(),
    }
  }
}

/// Represents common user activity states.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ActivityStatusPayload {
  Idle,
  /// User is actively viewing a specific resource.
  Viewing {
    resource_name: String,
  }, // e.g., "Document Page 5", "Task #123"
  /// User is actively editing a specific resource or field.
  Editing {
    resource_name: String,
  }, // e.g., "Section Title", "Spreadsheet Cell A1"
  /// User is actively typing (generic).
  Typing,
  /// Application-defined custom status.
  Custom {
    status_key: String,
    details: Option<String>,
  },
}

impl Default for ActivityStatusPayload {
  fn default() -> Self {
    ActivityStatusPayload::Idle
  }
}
