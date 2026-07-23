//! Hierarchical aggregation: standing in for a distant crowd with a single
//! summary, and only paying full detail for what is close.
//!
//! [`relevance`](crate::relevance) answers "who does this client need to know
//! about?", and its answer is binary: in the set or out of it. That is right for
//! things a client merely *draws*, and wrong for anything it has to *compute*
//! with, because dropping an input silently changes the answer. The measured
//! version of that in `blackhole_playground`: culling distant attractors by view
//! distance cut bandwidth by a third and multiplied the client's simulation error
//! by 2.4x, because gravity is long range and a hole you were not told about
//! still bends every pellet you hold.
//!
//! Aggregation is the third option between sending everything and sending
//! nothing: **keep the distant contribution, drop only its resolution**. Sixty
//! bodies on the far side of the arena pull almost exactly as one body of their
//! combined weight sitting at their centre of mass, and the further away they
//! are, the better that approximation gets. So the far half of the world
//! collapses to a handful of summaries while the near half stays exact.
//!
//! This is the Barnes-Hut construction, and the classic opening-angle criterion
//! is what decides where the line falls: a node standing `d` away with a cell
//! width of `s` may be summarized when `s / d < theta`. Small `theta` opens more
//! nodes and approaches exactness; large `theta` summarizes aggressively. It
//! costs O(n log n) to build and yields O(log n) summaries per viewpoint, so the
//! per-viewer wire cost and the per-item compute cost both stop tracking the
//! crowd size.
//!
//! Nothing here knows what a weight *is*. It is mass for a gravity field, but it
//! is equally a crowd's headcount for an LOD impostor, a cluster's threat for an
//! AI's target selection, or an accumulated noise level. The tree only requires
//! that the quantity be additive and that a distant group be adequately described
//! by its weighted centroid.
//!
//! ```
//! use plaza_server_utils::aggregate::{AggregateTree, WeightedPoint};
//!
//! let crowd: Vec<WeightedPoint> = (0..64)
//!   .map(|i| WeightedPoint::new(900.0 + (i % 8) as f32 * 10.0, 900.0 + (i / 8) as f32 * 10.0, 1.0))
//!   .collect();
//! let tree = AggregateTree::build(&crowd, 8);
//!
//! // Standing far away, the whole crowd is one summary carrying its full weight.
//! let mut far = Vec::new();
//! tree.summarize(0.0, 0.0, 0.7, &mut far);
//! assert_eq!(far.len(), 1);
//! assert_eq!(far[0].count, 64);
//!
//! // Standing inside it, the near members resolve individually.
//! let mut near = Vec::new();
//! tree.summarize(900.0, 900.0, 0.7, &mut near);
//! assert!(near.len() > far.len());
//! ```

/// One input to the tree: a position and an additive quantity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WeightedPoint {
  pub x: f32,
  pub y: f32,
  pub weight: f32,
}

impl WeightedPoint {
  pub fn new(x: f32, y: f32, weight: f32) -> Self {
    Self { x, y, weight }
  }
}

/// What a walk emits: either one input point, or a stand-in for a group of them.
///
/// `count == 1` means this is an exact member and nothing was approximated, which
/// is what lets a caller send the real entity rather than a summary of it. The
/// members are recoverable through [`AggregateTree::members`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
  /// The weighted centroid: where the group acts from.
  pub x: f32,
  pub y: f32,
  /// The group's total weight.
  pub weight: f32,
  /// How many inputs this stands for.
  pub count: u32,
  /// Width of the cell it came from, for a caller that wants to know how coarse
  /// the approximation is.
  pub size: f32,
  start: u32,
  len: u32,
}

const NO_CHILD: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
struct Node {
  com_x: f32,
  com_y: f32,
  weight: f32,
  size: f32,
  start: u32,
  len: u32,
  children: [u32; 4],
  leaf: bool,
}

/// A quadtree of weighted points, summarizable from any viewpoint.
///
/// Build once per tick over the whole set, then walk it once per viewer. The walk
/// is read-only, so one tree serves every client in a frame.
#[derive(Clone, Debug, Default)]
pub struct AggregateTree {
  nodes: Vec<Node>,
  order: Vec<u32>,
}

impl AggregateTree {
  /// Builds over `points`, deriving a square bounding cell from their extent.
  ///
  /// `max_depth` bounds the recursion, which matters because coincident points
  /// would otherwise subdivide forever. A leaf that hits the depth limit holds
  /// several points and is summarized as a group, which is the correct outcome:
  /// points that close together are not distinguishable at any useful distance.
  pub fn build(points: &[WeightedPoint], max_depth: u8) -> Self {
    if points.is_empty() {
      return Self::default();
    }

    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for p in points {
      min_x = min_x.min(p.x);
      min_y = min_y.min(p.y);
      max_x = max_x.max(p.x);
      max_y = max_y.max(p.y);
    }
    let size = (max_x - min_x).max(max_y - min_y).max(1.0);
    Self::build_in(points, ((min_x + max_x) * 0.5, (min_y + max_y) * 0.5), size, max_depth)
  }

  /// Builds over a **fixed** root cell rather than one derived from the points.
  ///
  /// Prefer this whenever the world has known bounds and the tree is rebuilt every
  /// tick over moving points. [`build`](Self::build) fits the cell to the current
  /// extent, so one entity wandering outward rescales and re-centres the entire
  /// subdivision: cluster membership then changes for reasons that have nothing to
  /// do with the entity being clustered, and a consumer integrating the summaries
  /// sees the field twitch every rebuild. A fixed cell makes the partition depend
  /// only on where things are, so a summary moves when its members move and at no
  /// other time.
  ///
  /// Points outside the cell are still included, and are sorted into the quadrant
  /// they fall toward; the tree stays correct, it just stops being balanced.
  pub fn build_in(points: &[WeightedPoint], center: (f32, f32), size: f32, max_depth: u8) -> Self {
    if points.is_empty() {
      return Self::default();
    }
    let mut order: Vec<u32> = (0..points.len() as u32).collect();
    let mut nodes = Vec::with_capacity(points.len() * 2);
    build_node(&mut nodes, &mut order, points, 0, center.0, center.1, size.max(1.0), 0, max_depth);
    Self { nodes, order }
  }

  /// Walks from `(x, y)`, emitting the coarsest set of summaries that satisfies
  /// the opening angle `theta`.
  ///
  /// A node is accepted when its cell width over its distance falls below
  /// `theta`; otherwise the walk descends into it. `theta <= 0.0` accepts nothing
  /// and therefore returns every input exactly, which is the useful off switch:
  /// the same code path with aggregation disabled, not a different one.
  ///
  /// `out` is cleared first and reused, so a per-frame walk allocates nothing
  /// after the first call.
  pub fn summarize(&self, x: f32, y: f32, theta: f32, out: &mut Vec<Summary>) {
    out.clear();
    if self.nodes.is_empty() {
      return;
    }
    // Explicit stack rather than recursion: the walk is the hot path and runs
    // once per viewer per tick.
    let mut stack = vec![0u32];
    while let Some(index) = stack.pop() {
      let node = &self.nodes[index as usize];
      if node.len == 0 {
        continue;
      }
      let (dx, dy) = (node.com_x - x, node.com_y - y);
      let dist = (dx * dx + dy * dy).sqrt();
      // A leaf is always accepted: there is nothing coarser to fall back to.
      if node.leaf || (theta > 0.0 && node.size < theta * dist) {
        out.push(Summary {
          x: node.com_x,
          y: node.com_y,
          weight: node.weight,
          count: node.len,
          size: node.size,
          start: node.start,
          len: node.len,
        });
      } else {
        for child in node.children {
          if child != NO_CHILD {
            stack.push(child);
          }
        }
      }
    }
  }

  /// The original input indices a summary stands for.
  pub fn members(&self, summary: &Summary) -> &[u32] {
    let start = summary.start as usize;
    &self.order[start..start + summary.len as usize]
  }

  /// How many points went in.
  pub fn len(&self) -> usize {
    self.order.len()
  }

  pub fn is_empty(&self) -> bool {
    self.order.is_empty()
  }
}

/// Builds one node over `order` (the indices belonging to this cell) and returns
/// its position in `nodes`.
#[allow(clippy::too_many_arguments)]
fn build_node(nodes: &mut Vec<Node>, order: &mut [u32], points: &[WeightedPoint], start: u32, cx: f32, cy: f32, size: f32, depth: u8, max_depth: u8) -> u32 {
  let (mut weight, mut wx, mut wy) = (0.0f32, 0.0f32, 0.0f32);
  for &i in order.iter() {
    let p = &points[i as usize];
    weight += p.weight;
    wx += p.x * p.weight;
    wy += p.y * p.weight;
  }
  // Weightless groups still have a position, so fall back to the plain centroid
  // rather than dividing by zero.
  let (com_x, com_y) = if weight.abs() > f32::EPSILON {
    (wx / weight, wy / weight)
  } else {
    let n = order.len().max(1) as f32;
    (order.iter().map(|&i| points[i as usize].x).sum::<f32>() / n, order.iter().map(|&i| points[i as usize].y).sum::<f32>() / n)
  };

  let index = nodes.len() as u32;
  nodes.push(Node {
    com_x,
    com_y,
    weight,
    size,
    start,
    len: order.len() as u32,
    children: [NO_CHILD; 4],
    leaf: true,
  });

  if order.len() <= 1 || depth >= max_depth {
    return index;
  }

  // Group the indices by quadrant so each child owns a contiguous run, which is
  // what makes `members` a slice rather than a gather.
  order.sort_unstable_by_key(|&i| quadrant(&points[i as usize], cx, cy));

  let quarter = size * 0.25;
  let mut children = [NO_CHILD; 4];
  let mut offset = 0usize;
  for q in 0..4u8 {
    let run = order[offset..].iter().take_while(|&&i| quadrant(&points[i as usize], cx, cy) == q).count();
    if run > 0 {
      let (ox, oy) = ((q & 1) as i32, (q >> 1) as i32);
      let child_cx = cx + if ox == 1 { quarter } else { -quarter };
      let child_cy = cy + if oy == 1 { quarter } else { -quarter };
      children[q as usize] = build_node(
        nodes,
        &mut order[offset..offset + run],
        points,
        start + offset as u32,
        child_cx,
        child_cy,
        size * 0.5,
        depth + 1,
        max_depth,
      );
      offset += run;
    }
  }

  nodes[index as usize].children = children;
  nodes[index as usize].leaf = false;
  index
}

fn quadrant(p: &WeightedPoint, cx: f32, cy: f32) -> u8 {
  (p.x >= cx) as u8 | (((p.y >= cy) as u8) << 1)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn grid(n: u32, spacing: f32, origin: f32) -> Vec<WeightedPoint> {
    (0..n * n)
      .map(|i| WeightedPoint::new(origin + (i % n) as f32 * spacing, origin + (i / n) as f32 * spacing, 1.0))
      .collect()
  }

  #[test]
  fn a_zero_angle_returns_every_point_exactly() {
    // The off switch has to be the same code path, or "aggregation disabled" is a
    // different implementation with its own bugs.
    let points = grid(8, 10.0, 100.0);
    let tree = AggregateTree::build(&points, 10);
    let mut out = Vec::new();
    tree.summarize(0.0, 0.0, 0.0, &mut out);
    assert_eq!(out.len(), points.len());
    assert!(out.iter().all(|s| s.count == 1));
  }

  #[test]
  fn total_weight_is_conserved_at_every_angle() {
    // The whole justification for aggregating rather than culling: the distant
    // contribution is kept, only its resolution is dropped.
    let points = grid(8, 10.0, 500.0);
    let total: f32 = points.iter().map(|p| p.weight).sum();
    let tree = AggregateTree::build(&points, 10);
    for theta in [0.0, 0.2, 0.5, 1.0, 4.0] {
      let mut out = Vec::new();
      tree.summarize(-2000.0, -2000.0, theta, &mut out);
      let sum: f32 = out.iter().map(|s| s.weight).sum();
      let counted: u32 = out.iter().map(|s| s.count).sum();
      assert!((sum - total).abs() < 0.01, "theta {theta}: weight {sum} against {total}");
      assert_eq!(counted, points.len() as u32, "theta {theta}: every point is accounted for exactly once");
    }
  }

  #[test]
  fn distance_decides_the_detail() {
    // The property that makes it useful for netcode: the same tree yields a small
    // summary set to a distant viewer and a detailed one to a close viewer, so per
    // recipient cost tracks what they can actually resolve.
    let points = grid(8, 12.0, 1000.0);
    let tree = AggregateTree::build(&points, 10);
    let (mut near, mut far) = (Vec::new(), Vec::new());
    tree.summarize(1040.0, 1040.0, 0.6, &mut near);
    tree.summarize(-5000.0, -5000.0, 0.6, &mut far);
    assert!(far.len() < near.len(), "far {} should be coarser than near {}", far.len(), near.len());
    assert_eq!(far.len(), 1, "from far enough away the whole crowd is one body");
  }

  #[test]
  fn a_summary_sits_at_the_weighted_centroid() {
    // Not the geometric centre: a heavy member pulls the stand-in toward itself,
    // which is what makes the approximation good rather than merely cheap.
    let points = vec![WeightedPoint::new(0.0, 0.0, 1.0), WeightedPoint::new(100.0, 0.0, 9.0)];
    let tree = AggregateTree::build(&points, 10);
    let mut out = Vec::new();
    tree.summarize(0.0, 100_000.0, 2.0, &mut out);
    assert_eq!(out.len(), 1);
    assert!((out[0].x - 90.0).abs() < 0.01, "centroid at {}", out[0].x);
    assert!((out[0].weight - 10.0).abs() < 0.01);
  }

  #[test]
  fn members_recover_the_inputs_a_summary_stands_for() {
    let points = grid(4, 20.0, 0.0);
    let tree = AggregateTree::build(&points, 10);
    let mut out = Vec::new();
    tree.summarize(-10_000.0, -10_000.0, 0.5, &mut out);
    let mut seen: Vec<u32> = out.iter().flat_map(|s| tree.members(s).iter().copied()).collect();
    seen.sort_unstable();
    assert_eq!(seen, (0..points.len() as u32).collect::<Vec<_>>());
  }

  #[test]
  fn coincident_points_terminate_at_the_depth_limit() {
    // Subdivision cannot separate identical positions, so without the bound the
    // build would recurse forever. They come back as one group, which is right.
    let points = vec![WeightedPoint::new(5.0, 5.0, 1.0); 32];
    let tree = AggregateTree::build(&points, 6);
    let mut out = Vec::new();
    tree.summarize(0.0, 0.0, 0.0, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].count, 32);
  }

  #[test]
  fn an_empty_set_summarizes_to_nothing() {
    let tree = AggregateTree::build(&[], 8);
    assert!(tree.is_empty());
    let mut out = vec![Summary { x: 1.0, y: 1.0, weight: 1.0, count: 1, size: 1.0, start: 0, len: 0 }];
    tree.summarize(0.0, 0.0, 0.5, &mut out);
    assert!(out.is_empty(), "the output is cleared even when there is nothing to walk");
  }

  #[test]
  fn the_summary_count_grows_slowly_with_the_crowd() {
    // The scaling claim. Doubling the crowd should not double what a viewer is
    // told about it, or aggregation has bought nothing.
    let mut counts = Vec::new();
    for n in [4u32, 8, 16] {
      let points = grid(n, 30.0, 0.0);
      let tree = AggregateTree::build(&points, 12);
      let mut out = Vec::new();
      tree.summarize(-1500.0, -1500.0, 0.5, &mut out);
      counts.push((points.len(), out.len()));
    }
    let (n_small, s_small) = counts[0];
    let (n_big, s_big) = counts[2];
    assert!(n_big / n_small == 16);
    assert!(s_big < s_small * 4, "summaries {s_small} to {s_big} while the crowd grew 16x");
  }
}
