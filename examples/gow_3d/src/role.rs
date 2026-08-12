playground_common::playground_role!(port: 8301);

/// Pulls `--bots N` out of argv before the shared parser sees it.
///
/// The shared parser refuses an option it does not know, which is the right
/// default for every other example: a typo should not be ignored. This one flag
/// belongs to this crate alone, so it is taken here rather than added to a
/// vocabulary every playground would then carry.
pub fn take_bots(args: impl IntoIterator<Item = String>) -> (Vec<String>, usize) {
  let mut kept = Vec::new();
  let mut bots = crate::bots_default();
  let mut args = args.into_iter();
  while let Some(arg) = args.next() {
    if arg == "--bots" {
      if let Some(value) = args.next() {
        bots = value.parse().unwrap_or(bots);
      }
    } else {
      kept.push(arg);
    }
  }
  (kept, bots)
}
