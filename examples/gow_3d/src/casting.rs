//! Cast bars and the global cooldown.
//!
//! An ability with a cast time hides its round trip, because the player is
//! already waiting. What they notice is the **fraction of the wait that was
//! delay** rather than the delay itself and a cast bar makes that fraction
//! small.
//!
//! Nothing here predicts, reconciles or interpolates. The client starts a bar
//! when the key goes down and the server says what happened; the gap between
//! the bar finishing and the answer arriving is the only exposure. A longer
//! cast does not shrink that gap, but it makes the gap a smaller share of the
//! wait.
//!
//! puck_rink uses rollback to hide a hundred milliseconds on five bodies. Here
//! the game design solves the same problem instead.

/// Milliseconds an ability takes to go off.
pub type Ms = u64;

/// What one press looks like from where the player is sitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Perceived {
  /// When the client's own bar finished, counting from the press.
  pub bar_done: Ms,
  /// When the server's answer arrived.
  pub answer: Ms,
  /// The wait after the bar finished, which is the only part a player can
  /// attribute to the network.
  pub exposed: Ms,
}

impl Perceived {
  /// The share of the whole wait that was exposure.
  ///
  /// A hundred and fifty milliseconds is the whole wait for an instant ability
  /// and a tenth of the wait for a one-and-a-half second cast.
  pub fn share(&self) -> f32 {
    if self.answer == 0 {
      return 0.0;
    }
    self.exposed as f32 / self.answer as f32
  }
}

/// One press, with the server committing at the end of the cast.
///
/// The client presses at zero; the server hears at half a round trip, runs the
/// cast, and answers, which arrives half a round trip later. Meanwhile the
/// client's own bar ran from the press, so it finished at the cast time.
pub fn press(cast_ms: Ms, rtt_ms: Ms) -> Perceived {
  let one_way = rtt_ms / 2;
  let answer = one_way + cast_ms + one_way;
  Perceived {
    bar_done: cast_ms,
    answer,
    exposed: answer.saturating_sub(cast_ms),
  }
}

/// The global cooldown, which covers inputs the way a cast covers the outcome.
///
/// A player who cannot act again for this long has no next input that needs
/// frame-accurate timing, so nothing has to be predicted to keep it responsive.
/// That is why an instant ability in this genre is still not a latency problem.
pub const GLOBAL_COOLDOWN_MS: Ms = 1500;

/// Whether an input arriving this late still lands inside the window the design
/// already made the player wait through.
pub fn within_the_wait(cast_ms: Ms, rtt_ms: Ms) -> bool {
  rtt_ms <= cast_ms.max(GLOBAL_COOLDOWN_MS)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_longer_cast_dilutes_the_delay_rather_than_hiding_it() {
    // The exposure is the same hundred and fifty milliseconds at every cast
    // time. Only its fraction of the wait changes.
    let rtt = 150;
    for cast in [0u64, 500, 1500] {
      let p = press(cast, rtt);
      assert_eq!(p.exposed, rtt, "the delay itself never shrinks");
    }

    assert!(press(0, rtt).share() > 0.99, "an instant ability is all delay");
    assert!(press(1500, rtt).share() < 0.11, "and a cast bar is a tenth of one");
  }

  #[test]
  fn what_a_cast_bar_is_worth() {
    println!("\n  the share of the wait a player can blame on the network:\n");
    println!("{:>10} {:>10} {:>10} {:>10}", "cast", "rtt 30", "rtt 150", "rtt 300");
    for cast in [0u64, 400, 1000, 1500, 2500] {
      println!(
        "{cast:>10} {:>9.0}% {:>9.0}% {:>9.0}%",
        press(cast, 30).share() * 100.0,
        press(cast, 150).share() * 100.0,
        press(cast, 300).share() * 100.0,
      );
    }
    println!("\n  puck_rink spends a rollback apparatus to hide 100ms on five bodies.\n  this asks the designer for a second and a half instead.\n");

    // At a cast time the genre uses, a bad connection is a smaller share of the
    // wait than a good connection is for an instant ability.
    let instant_on_a_good_line = press(0, 30).share();
    let cast_on_a_bad_one = press(1500, 300).share();
    assert!(
      cast_on_a_bad_one < instant_on_a_good_line,
      "{cast_on_a_bad_one} against {instant_on_a_good_line}"
    );
  }

  #[test]
  fn the_global_cooldown_covers_what_a_cast_time_does_not() {
    // An instant ability is still covered, because the player cannot act again
    // for a second and a half.
    assert!(within_the_wait(0, 300), "an instant on a bad line is still inside the cooldown");
    assert!(!within_the_wait(0, 2000), "and two seconds is not a game, it is a fault");
  }
}
