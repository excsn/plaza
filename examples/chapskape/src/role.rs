playground_common::playground_role!(port: 8302);

/// Pulls `--bots N` out of argv before the shared parser sees it.
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
