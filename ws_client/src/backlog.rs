//! Discarding a resume backlog before any of it is parsed.
//!
//! A hidden browser tab (or a machine that slept) stops running frames while
//! its socket keeps receiving, so the first [`Socket::poll`] after it wakes
//! can hand back minutes of traffic at once. None of it is playable: a client
//! that renders in the past is about to restart its timeline, which discards
//! whatever those messages would have built. Parsing them anyway is where a
//! several-second freeze on refocus comes from, so the drop happens here, on
//! message lengths alone, before any deserialisation.
//!
//! What this cannot know is whether the burst is a *resume* or a *join*: a
//! fresh connection's first poll legitimately carries a welcome and a warm
//! world's whole baseline, and that must arrive intact. The caller knows (it
//! has seen a frame before, or it has not), which is why this is a function
//! the application calls rather than something [`Socket::poll`] does on its
//! own.
//!
//! Dropping unread is safe only under the recovery contract the plaza blocks
//! implement: the client restarts its timeline and drops its mirror, its next
//! acknowledgement carries the digest of nothing, and the server answers with
//! a full baseline. A transport used without that contract should not use
//! this.
//!
//! [`Socket::poll`]: crate::Socket::poll

use crate::Event;

/// What a trim discarded, for the application's meters and panel. The bytes
/// still crossed the wire: count them as received, because the meters measure
/// the link, not what the client chose to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DroppedBacklog {
  pub messages: u64,
  pub bytes: u64,
}

/// Trims a drained event list down to its newest `keep` payload messages, if
/// it holds more than `trigger` of them.
///
/// `None` means the list was an ordinary poll and is untouched. `Some` means
/// it was a backlog: everything but the newest `keep` messages is gone, the
/// caller should treat its timeline as lost, and the return value says what
/// was discarded. [`Event::Open`] and [`Event::Closed`] are never dropped,
/// because they carry the connection's own state.
///
/// Pick `trigger` several times past what a running frame loop can accumulate
/// between two polls (a few seconds of the stream's message rate), and `keep`
/// around what one send interval holds, so the restarted timeline has
/// something current to anchor on.
pub fn trim_backlog(events: &mut Vec<Event>, trigger: usize, keep: usize) -> Option<DroppedBacklog> {
  let payload_len = |e: &Event| match e {
    Event::Message(bytes) => Some(bytes.len()),
    Event::Text(text) => Some(text.len()),
    Event::Open | Event::Closed(_) => None,
  };
  let backlog = events.iter().filter(|e| payload_len(e).is_some()).count();
  if backlog <= trigger {
    return None;
  }
  let drop_first = backlog - keep.min(backlog);
  let mut dropped = DroppedBacklog { messages: 0, bytes: 0 };
  let mut seen = 0usize;
  events.retain(|e| {
    let Some(len) = payload_len(e) else {
      return true;
    };
    seen += 1;
    if seen > drop_first {
      return true;
    }
    dropped.messages += 1;
    dropped.bytes += len as u64;
    false
  });
  Some(dropped)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::CloseReason;

  fn msg(n: usize) -> Event {
    Event::Message(vec![0u8; n])
  }

  #[test]
  fn an_ordinary_poll_is_untouched() {
    let mut events = vec![Event::Open, msg(10), msg(10)];
    assert_eq!(trim_backlog(&mut events, 8, 4), None);
    assert_eq!(events.len(), 3);
  }

  #[test]
  fn a_backlog_keeps_the_newest_and_reports_the_rest() {
    let mut events: Vec<Event> = (0..20).map(|i| Event::Text(format!("m{i:02}"))).collect();
    let dropped = trim_backlog(&mut events, 8, 4).expect("20 messages is a backlog");
    assert_eq!(dropped.messages, 16);
    assert_eq!(dropped.bytes, 16 * 3, "counted by length, never parsed");
    let kept: Vec<_> = events
      .iter()
      .map(|e| match e {
        Event::Text(t) => t.as_str(),
        _ => panic!("payload only"),
      })
      .collect();
    assert_eq!(kept, ["m16", "m17", "m18", "m19"], "the tail survives, in order");
  }

  #[test]
  fn connection_state_events_are_never_dropped() {
    let mut events = vec![Event::Open];
    events.extend((0..20).map(|_| msg(5)));
    events.push(Event::Closed(CloseReason::Local));
    let dropped = trim_backlog(&mut events, 8, 2).expect("a backlog");
    assert_eq!(dropped.messages, 18);
    assert!(matches!(events.first(), Some(Event::Open)), "the open survives in place");
    assert!(matches!(events.last(), Some(Event::Closed(_))), "so does the close");
    assert_eq!(events.len(), 4);
  }
}
