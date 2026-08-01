import 'dart:collection';

/// A storage slot and the generation of its current occupant.
///
/// A server keeping entities in a dense recycled array has the cheapest possible
/// identifier in the array index, and it is wrong in a way that is very hard to
/// see. Slot 41 dies and is refilled on the same tick; every message about the
/// old occupant still in flight now names the new one, and neither side can
/// notice, because the index is valid and the message is well formed. A
/// generation counter makes the mismatch visible at the point of use.
///
/// **Both sides must encode the pair identically**, or their digests disagree
/// about a world they hold identically and the recovery machinery fires for ever
/// chasing arithmetic. That is why this is a type rather than two agreeing
/// comments, and why [encode] is pinned by a conformance fixture.
///
/// Ported from `plaza_client_utils::slot::SlotKey`, which `plaza_server_utils`
/// re-exports so the two Rust sides cannot diverge.
class SlotKey {
  const SlotKey(this.index, this.generation);

  /// Which slot. Dense and reused, so not an identity by itself.
  final int index;

  /// How many times this slot has been refilled. What makes the pair an identity.
  final int generation;

  /// Packs to the value that goes on the wire and into a digest.
  ///
  /// The index shifts by 16 to leave the generation room, so an index past 2^48
  /// would collide. Nothing this is for comes close.
  int encode() => (index << 16) | generation;

  static SlotKey decode(int key) => SlotKey(key >>> 16, key & 0xFFFF);

  /// The same slot with its generation dropped.
  ///
  /// For running deliberately without generations, which is how you demonstrate
  /// what they are for: every reference then matches whatever is in the slot,
  /// which is precisely the bug, made visible on demand.
  SlotKey ungenerational() => SlotKey(index, 0);

  /// Whether [other] names the same slot *and* the same occupant.
  bool sameOccupant(SlotKey other) => index == other.index && generation == other.generation;

  @override
  bool operator ==(Object other) =>
      other is SlotKey && other.index == index && other.generation == generation;

  @override
  int get hashCode => Object.hash(index, generation);

  @override
  String toString() => 'SlotKey($index, gen $generation)';
}

/// The order freed slots come back in.
///
/// Neither is more correct. Prefer [lifo] unless something downstream cares
/// about clustering, and if it does, measure rather than assume.
enum ReusePolicy {
  /// Newest freed first. A stack, so the working set stays compact and recycled
  /// indices scatter in whatever order things died.
  lifo,

  /// Oldest freed first. A queue, so a slot rests as long as possible before
  /// reuse, widening the window before a generation can wrap.
  fifo,
}

/// Hands out [SlotKey]s over a dense index space, recycling freed slots and
/// bumping a generation so stale handles stay detectable.
///
/// **It does not store your entities.** Keep them in a list indexed by
/// [SlotKey.index], which is what the rest of these utilities expect.
///
/// # The ceiling, stated out loud
///
/// The generation is 16 bits, so a single slot freed 65,536 times wraps and a
/// handle from exactly that many reuses ago aliases the current occupant.
/// Nothing can detect the wrap, so the mitigation is width and, for a long
/// session, [ReusePolicy.fifo] to spread reuse across the index space instead of
/// hammering the same slots.
///
/// Ported from `plaza_client_utils::slot::SlotAllocator`.
class SlotAllocator {
  SlotAllocator({this.policy = ReusePolicy.lifo});

  ReusePolicy policy;

  /// Per index: the current generation, and whether it is occupied.
  final List<int> _generation = <int>[];
  final List<bool> _occupied = <bool>[];
  final Queue<int> _free = Queue<int>();
  int _live = 0;

  /// Takes a slot, reusing a freed index when one is available.
  ///
  /// Indices are dense: the space only grows when nothing is free, so it settles
  /// at the high-water mark of simultaneously live entities rather than at the
  /// total ever created.
  SlotKey alloc() {
    _live++;
    if (_free.isEmpty) {
      final index = _generation.length;
      _generation.add(0);
      _occupied.add(true);
      return SlotKey(index, 0);
    }
    final index = policy == ReusePolicy.lifo ? _free.removeLast() : _free.removeFirst();
    _occupied[index] = true;
    return SlotKey(index, _generation[index]);
  }

  /// Releases a slot, if this key names its current occupant.
  ///
  /// Returns whether it freed anything: a key naming an occupant already gone is
  /// refused rather than freeing whoever moved in, the same check
  /// [DeltaMirror.remove] makes on the other side of the wire.
  ///
  /// The generation is bumped **here**, on free, rather than when the slot is
  /// next taken. An outstanding handle should stop naming anything the moment its
  /// subject dies, not whenever something happens to want the index. Between
  /// those two moments is exactly the window a delta stream re-derives
  /// retractions in.
  bool free(SlotKey key) {
    if (key.index < 0 || key.index >= _occupied.length) return false;
    if (!_occupied[key.index] || _generation[key.index] != key.generation) return false;
    _occupied[key.index] = false;
    _generation[key.index] = (_generation[key.index] + 1) & 0xFFFF;
    _free.addLast(key.index);
    _live--;
    return true;
  }

  bool isLive(SlotKey key) =>
      key.index >= 0 &&
      key.index < _occupied.length &&
      _occupied[key.index] &&
      _generation[key.index] == key.generation;

  bool isOccupied(int index) => index >= 0 && index < _occupied.length && _occupied[index];

  /// The key naming whoever currently occupies [index].
  ///
  /// For going the other way: application state is stored by index, so this is
  /// how a bare index becomes a handle that can go on the wire.
  SlotKey? keyAt(int index) =>
      isOccupied(index) ? SlotKey(index, _generation[index]) : null;

  /// Every live key, in index order.
  Iterable<SlotKey> get keys sync* {
    for (var i = 0; i < _occupied.length; i++) {
      if (_occupied[i]) yield SlotKey(i, _generation[i]);
    }
  }

  int get length => _live;
  bool get isEmpty => _live == 0;

  /// How many indices exist, live or free.
  ///
  /// The width application storage must cover, so this and not [length] is the
  /// number to size a list by.
  int get indexSpace => _generation.length;

  /// Frees every slot, bumping each live generation so outstanding handles are
  /// invalidated rather than silently matching a rebuilt world. Keeps the index
  /// space, so storage indexed by it stays valid.
  void clear() {
    _free.clear();
    for (var i = 0; i < _occupied.length; i++) {
      if (_occupied[i]) {
        _occupied[i] = false;
        _generation[i] = (_generation[i] + 1) & 0xFFFF;
      }
      _free.addLast(i);
    }
    _live = 0;
  }
}
