pub mod tick_scheduler;
pub mod time_scheduler;
pub mod tick_callback_scheduler;
pub mod time_callback_scheduler;

pub use tick_scheduler::TickEventScheduler;
pub use time_scheduler::TimeEventScheduler;
pub use tick_callback_scheduler::TickCallbackScheduler;
pub use time_callback_scheduler::TimeCallbackScheduler;

/// Unique identifier for a scheduled event, allowing cancellation.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScheduledEventId(u64);