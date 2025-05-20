//! Common utility types used throughout the `plaza_client_utils` crate.

/// Represents a sequence number, typically for client inputs or server state updates.
///
/// This number should generally be monotonically increasing for each stream of
/// inputs or updates to which it pertains. It's crucial for ordering inputs
/// during client-side prediction replay and for identifying which inputs have been
/// acknowledged by the server during reconciliation.
pub type SequenceNumber = u64;

/// Represents client-side time, often in milliseconds since a client-defined epoch
/// (e.g., application start, connection establishment).
///
/// Used for timestamping client-generated inputs or for managing client-side
/// timed events. Its exact meaning and resolution are up to the client application.
pub type ClientTimeMs = u64;

// If we find other very common, simple types needed across modules, they can be added here.
// For example, if dealing with 2D/3D coordinates frequently and not wanting to pull in a full
// math library as a direct dependency of plaza_client_utils itself, simple Vec2/Vec3 structs
// could be defined here (though it's often better to make utilities generic over such types
// and let the application provide them from its chosen math library).