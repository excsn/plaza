/// A sequence number, typically for client inputs or server state updates.
///
/// Monotonically increasing per stream. It is what orders inputs during
/// prediction replay and what identifies which inputs the server has
/// acknowledged during reconciliation.
typedef SequenceNumber = int;

/// Client-side time in milliseconds since a client-defined epoch, usually
/// application start or connection establishment. Its exact meaning and
/// resolution are the application's.
typedef ClientTimeMs = int;
