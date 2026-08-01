import 'ack.dart';
import 'digest.dart';
import 'slot.dart';

/// Whether the mirror matches what the server says it should be.
sealed class Agreement {
  const Agreement();
  bool get agreed => this is Agreed;
}

class Agreed extends Agreement {
  const Agreed();
}

/// Carries both digests so a report can say so, though neither number says
/// *how*. For that, see [DeltaMirror.divergenceFrom].
class Diverged extends Agreement {
  const Diverged({required this.held, required this.expected});
  final int held;
  final int expected;
}

/// Which way a mirror diverged, given the server's own key set.
///
/// A digest detects a divergence and cannot diagnose one, so anything shipping a
/// digest wants a mode that ships the ground truth beside it. Which side the
/// difference falls on names the bug: [missing] means something was lost or never
/// sent, [extra] means a removal never landed or was rejected.
class Divergence {
  const Divergence({required this.extra, required this.missing});

  /// Occupants this mirror holds that the server does not.
  final List<SlotKey> extra;

  /// Occupants the server expects that this mirror does not hold.
  final List<SlotKey> missing;

  bool get isEmpty => extra.isEmpty && missing.isEmpty;
}

class _Held<E> {
  _Held(this.generation, this.entity);
  int generation;
  E entity;
}

/// The client's mirror of a streamed entity set.
///
/// A delta stream has a silent failure mode: apply the entered and left deltas
/// to keep a local set, and if one is lost or misapplied the set is wrong for
/// good, with no symptom. This holds the keying, the agreement and the counters;
/// the entity type is yours, so an application keeps whatever it needs per
/// entity beside it.
///
/// **This must agree with the server's `DeltaBaseline` exactly.** In Rust that is
/// guaranteed by `plaza_server_utils` re-exporting this very type. A Dart port
/// cannot inherit that guarantee, so the operations are pinned by conformance
/// fixtures instead.
///
/// Ported from `plaza_client_utils::mirror::DeltaMirror`.
class DeltaMirror<E> {
  DeltaMirror({this.generational = true});

  /// With generations off, every reference matches whatever occupies the slot,
  /// which is the bug generations exist to prevent, available on demand so it can
  /// be demonstrated.
  bool generational;

  final Map<int, _Held<E>> _held = <int, _Held<E>>{};
  final AckWindow _acks = AckWindow();
  int? _appliedSeq;
  int _digest = 0;
  int _framesLost = 0;
  int _staleRefs = 0;
  int _divergences = 0;

  SlotKey _normalise(SlotKey key) => generational ? key : key.ungenerational();

  /// Opens a packet: notes what the wire lost, acknowledges the sequence, and
  /// clears the mirror if this packet is a full baseline.
  ///
  /// Call once per packet, before applying anything in it. A full baseline is the
  /// server's repair for a mirror it can no longer reach by deltas, so the old
  /// contents must go rather than be merged with: merging is what leaves the
  /// drift that prompted the rebuild.
  void begin(int seq, {required bool fullBaseline}) {
    // Every frame is numbered and the link is ordered, so a jump of more than
    // one means the wire lost what was between. This is the direct measure, and
    // what separates "the network dropped it" from "we corrupted it".
    final previous = _appliedSeq;
    if (previous != null && seq > previous + 1) {
      _framesLost += seq - previous - 1;
    }
    if (previous == null || seq > previous) _appliedSeq = seq;
    _acks.observe(seq);
    if (fullBaseline) _held.clear();
  }

  /// Files an entity under a key, replacing whatever was in the slot.
  ///
  /// A fresh occupant of a reused slot is exactly this: the previous tenant is
  /// gone and the generation recorded is the new one.
  void insert(SlotKey key, E entity) {
    final k = _normalise(key);
    _held[k.index] = _Held<E>(k.generation, entity);
  }

  /// Removes an occupant, if this key names the one actually held.
  ///
  /// A generation mismatch counts as a stale reference and removes nothing, which
  /// is the entire point: without the check this deletes a live entity that
  /// merely inherited the slot.
  E? remove(SlotKey key) {
    final k = _normalise(key);
    final held = _held[k.index];
    if (held == null) return null;
    if (held.generation != k.generation) {
      _staleRefs++;
      return null;
    }
    _held.remove(k.index);
    return held.entity;
  }

  E? operator [](SlotKey key) {
    final k = _normalise(key);
    final held = _held[k.index];
    if (held == null || held.generation != k.generation) return null;
    return held.entity;
  }

  /// The occupant this key names, counting a generation mismatch as a stale
  /// reference.
  ///
  /// For applying a sample. Returning null rather than the current occupant is
  /// what keeps a position meant for a dead entity off a live one.
  E? forUpdate(SlotKey key) {
    final k = _normalise(key);
    final held = _held[k.index];
    if (held == null) return null;
    if (held.generation != k.generation) {
      _staleRefs++;
      return null;
    }
    return held.entity;
  }

  /// Replaces the entity under a key, if the key names the held occupant.
  bool update(SlotKey key, E entity) {
    final k = _normalise(key);
    final held = _held[k.index];
    if (held == null) return false;
    if (held.generation != k.generation) {
      _staleRefs++;
      return false;
    }
    held.entity = entity;
    return true;
  }

  bool contains(SlotKey key) => this[key] != null;

  /// Closes a packet: recomputes the digest and compares it to the server's.
  ///
  /// Everything in the packet has been applied by now, so the mirror must match
  /// what the server said it should be. This is the check a lost or malformed
  /// removal cannot hide from, because it is over the whole set rather than over
  /// the messages that happened to arrive.
  Agreement settle(int expected) {
    _digest = computeDigest();
    if (_digest == expected) return const Agreed();
    _divergences++;
    return Diverged(held: _digest, expected: expected);
  }

  /// How this mirror differs from the server's own key set.
  ///
  /// For the debugging mode that ships the truth beside the digest. Cheap enough
  /// to call on a mismatch, far too expensive to send every packet, which is why
  /// it takes the keys rather than assuming they are always there.
  Divergence divergenceFrom(Iterable<int> serverKeys) {
    final theirs = serverKeys.toSet();
    final mine = keys.map((k) => k.encode()).toSet();
    final extra = mine.difference(theirs).map(SlotKey.decode).toList()
      ..sort((a, b) => a.encode().compareTo(b.encode()));
    final missing = theirs.difference(mine).map(SlotKey.decode).toList()
      ..sort((a, b) => a.encode().compareTo(b.encode()));
    return Divergence(extra: extra, missing: missing);
  }

  /// The digest as of the last [settle]. Send this on the next acknowledgement:
  /// the server compares it against the state it believes this client reached,
  /// which is the only way it can detect a drift its own deltas cannot repair.
  int get digest => _digest;

  /// The digest of everything held right now.
  int computeDigest() => SetDigest.fromKeys(keys.map((k) => k.encode())).digest;

  AckWindow get acks => _acks;

  /// Live keys, in index order.
  Iterable<SlotKey> get keys {
    final indices = _held.keys.toList()..sort();
    return indices.map((i) => SlotKey(i, _held[i]!.generation));
  }

  Iterable<(SlotKey, E)> get entries =>
      keys.map((k) => (k, _held[k.index]!.entity));

  Iterable<E> get values => keys.map((k) => _held[k.index]!.entity);

  int get length => _held.length;
  bool get isEmpty => _held.isEmpty;

  void clear() => _held.clear();

  /// Frames the wire lost, from gaps in the sequence.
  int get framesLost => _framesLost;

  /// References to occupants that had already gone. A climbing count means the
  /// server is naming entities this mirror has moved past.
  int get staleRefs => _staleRefs;

  /// Times [settle] disagreed with the server.
  int get divergences => _divergences;

  int? get appliedSeq => _appliedSeq;
}
