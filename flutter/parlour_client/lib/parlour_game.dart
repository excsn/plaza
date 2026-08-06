import 'dart:async';
import 'dart:ui' show AppLifecycleState;

import 'package:flame/game.dart';
import 'package:plaza_flame/plaza_flame.dart';

import 'sequencer.dart';

/// A table as the lobby lists it.
class TableCard {
  const TableCard({required this.roomId, required this.name, required this.players, required this.seats});

  factory TableCard.fromFields(Map<String, Object?> f) => TableCard(
        roomId: f['room_id'] as String,
        name: f['name'] as String,
        players: f['current_players'] as int,
        seats: f['max_players'] as int,
      );

  final String roomId;
  final String name;
  final int players;
  final int seats;
}

/// What this client was told it may see.
///
/// Mirrors the server's `PlayerView`, which is built per recipient: your own
/// cards by rank, everyone else's by count. The absence of other hands is not a
/// rendering decision here, it is what arrived.
class TableView {
  TableView.fromFields(Map<String, Object?> f)
      : table = f['table'] as String,
        phase = f['phase'] as String,
        round = f['round'] as int,
        totalRounds = f['total_rounds'] as int?,
        whoseTurn = f['whose_turn'] as int?,
        yourSeat = f['your_seat'] as String?,
        coins = f['coins'] as int,
        myHand = (f['my_hand'] as List<Object?>).cast<int>().toList(),
        opponents = (f['opponents'] as List<Object?>)
            .map((e) => (e as List<Object?>).cast<int>())
            .map((p) => (player: p[0], cards: p[1]))
            .toList(),
        played = (f['played'] as List<Object?>)
            .map((e) => (e as List<Object?>).cast<int>())
            .map((p) => (player: p[0], card: p[1]))
            .toList(),
        scores = (f['scores'] as List<Object?>)
            .map((e) => (e as List<Object?>).cast<int>())
            .map((p) => (player: p[0], tricks: p[1]))
            .toList(),
        seatsTaken = f['seats_taken'] as int,
        seatsTotal = f['seats_total'] as int,
        bots = f['bots'] as int;

  final String table;
  String phase;
  int round;
  final int? totalRounds;
  int? whoseTurn;
  final String? yourSeat;
  int coins;
  final List<int> myHand;
  final List<({int player, int cards})> opponents;
  final List<({int player, int card})> played;
  final List<({int player, int tricks})> scores;
  final int seatsTaken;
  final int seatsTotal;
  final int bots;

  bool get spectating => yourSeat == 'Spectator';
}

/// How long each kind of op is worth watching, in seconds.
///
/// A card landing wants to be seen; a phase change does not. These live here
/// rather than in [OpSequencer] because the sequencer has no idea what an op is,
/// which is what keeps it reusable.
class TablePacing {
  const TablePacing({this.cardPlayed = 0.45, this.trickWon = 0.9, this.settled = 1.5});

  final double cardPlayed;
  final double trickWon;
  final double settled;
}

/// A card game across two connections.
///
/// **The lobby socket stays open.** It is tempting to close it once `Placed`
/// arrives and the table endpoint is in hand, and that is exactly wrong: the
/// server reads a closed lobby socket as the player giving up, withdraws the
/// reservation it just issued, and the table seats them as a spectator. The two
/// sockets have separate lifetimes and the first one gates the second.
class ParlourGame extends FlameGame with PlazaGame {
  ParlourGame({
    required this.lobbyUrl,
    required this.connect,
    this.protocol = ProtocolVersion.unknown,
    this.pacing = const TablePacing(),
  });

  final Uri lobbyUrl;
  final SocketFactory connect;
  final ProtocolVersion protocol;
  final TablePacing pacing;

  /// The second connection. Null until the lobby places this player, and
  /// replaced rather than reused if they are placed again.
  PlazaClient? _table;
  StreamSubscription<Object?>? _tableOps;
  StreamSubscription<PlazaEvent>? _tableEvents;

  final OpSequencer sequencer = OpSequencer();

  final List<TableCard> tables = <TableCard>[];
  int? playerId;
  int coins = 0;
  String status = 'connecting';
  String? queued;
  TableView? view;
  final List<String> log = <String>[];

  bool get seated => _table != null;
  PlazaStatus get tableStatus => _table?.status ?? PlazaStatus.closed;

  @override
  PlazaClient createClient() => PlazaClient(
        url: lobbyUrl,
        connect: connect,
        protocol: protocol,
        backoff: Backoff(initial: const Duration(milliseconds: 400)),
      );

  /* ------------------------------------------------------------ the lobby */

  void quickMatch() => sendPlazaOp(variant('QuickMatch'));
  void leaveQueue() => sendPlazaOp(variant('LeaveQueue'));
  void listTables() => sendPlazaOp(variant('ListTables'));
  void spectate(String roomId) => sendPlazaOp(variant('Spectate', {'room_id': roomId}));

  @override
  void onPlazaOp(Object? op) {
    // `variantName` and `variantFields`, never `op['Catalogue']`: a unit variant
    // arrives as a bare string and indexing drops it silently. `QueueLeft` is
    // exactly that case.
    final f = variantFields(op);
    switch (variantName(op)) {
      case 'Welcome':
        playerId = f['you'] as int;
        coins = f['coins'] as int;
        status = 'in the lobby';
      case 'Catalogue':
        tables
          ..clear()
          ..addAll((f['tables'] as List<Object?>).cast<Map<String, Object?>>().map(TableCard.fromFields));
      case 'Queued':
        queued = 'place ${(f['position'] as int) + 1} of ${f['needed']}';
        status = 'queued';
      case 'QueueLeft':
        queued = null;
        status = 'in the lobby';
      case 'Placed':
        queued = null;
        coins = f['coins'] as int;
        status = 'placed at ${f['name']}';
        _note('placed at ${f['name']}');
        unawaited(openTable(Uri.parse(f['endpoint'] as String)));
      case 'Refused':
        queued = null;
        status = 'refused: ${f['reason']}';
        _note('refused: ${f['reason']}');
    }
  }

  /* ------------------------------------------------------------ the table */

  /// Opens the room socket the lobby just named.
  ///
  /// The endpoint carries a one-use ticket, so this must be dialled once and a
  /// second attempt with the same URL is refused by the server. Replacing an
  /// existing table connection closes it first, and its subscriptions go with
  /// it, or a stale socket keeps feeding the sequencer.
  Future<void> openTable(Uri endpoint) async {
    await closeTable();

    final client = PlazaClient(
      url: endpoint,
      connect: connect,
      // The table speaks named MessagePack where the lobby speaks JSON. One
      // codec class reads both shapes, so what changes here is the encoding, not
      // the reading of it.
      codec: const MsgPackCodec(),
      protocol: protocol,
      backoff: Backoff(initial: const Duration(milliseconds: 400)),
    );
    _table = client;
    _tableOps = client.ops.listen((op) {
      plazaStats.countIn();
      sequencer.add(op);
    });
    _tableEvents = client.events.listen((e) {
      plazaStats.apply(e, client.status);
      if (e is Connected && e.resumed) {
        // Fresh state is coming; a queued backlog would animate a world that
        // has already moved on.
        sequencer.clear();
      }
      onPlazaEvent(e);
    });
    await client.start();
  }

  Future<void> closeTable() async {
    final client = _table;
    _table = null;
    view = null;
    sequencer.clear();
    unawaited(_tableOps?.cancel());
    unawaited(_tableEvents?.cancel());
    _tableOps = null;
    _tableEvents = null;
    if (client != null) await client.stop();
  }

  /// Sends to the table, not the lobby. Returns false when the table socket is
  /// not open, which is the caller's cue that the play was dropped rather than
  /// queued: replaying a card after a reconnect is how one gets played twice.
  bool playCard(int rank) {
    final client = _table;
    if (client == null) return false;
    final ok = client.sendOps(<Object?>[variant('PlayCard', rank)]);
    if (ok) plazaStats.countOut(1);
    return ok;
  }

  bool get myTurn => view != null && view!.whoseTurn == playerId && view!.phase == 'Playing';

  /// Applies one op and says how long it is worth watching.
  ///
  /// A snapshot arrives on a deal and a resolved trick and nothing in between,
  /// so the rest of the round is narrated as ops and this is the half of that
  /// bargain the client owes.
  double applyTableOp(Object? op) {
    final f = variantFields(op);
    final v = view;
    switch (variantName(op)) {
      case 'Snapshot':
        view = TableView.fromFields(variantBody(op)! as Map<String, Object?>);
        return 0;

      case 'PhaseChanged':
        v?.phase = f['new_phase'] as String;
        return 0;

      case 'TurnChanged':
        v?.whoseTurn = f['new_turn_actor'] as int?;
        return 0;

      case 'RoundStarted':
        v?.round = f['round_number'] as int;
        return 0;

      case 'CardPlayed':
      case 'PlayedForYou':
        _played(f['player'] as int, f['card'] as int);
        _note('#${f['player']} played ${f['card']}');
        return pacing.cardPlayed;

      case 'TrickWon':
        _note('#${f['player']} took the trick with ${f['card']}');
        return pacing.trickWon;

      case 'Settled':
        coins = f['coins'] as int;
        _note('match over, #${f['winner']} takes the stake');
        return pacing.settled;

      case 'Rejected':
        _note('refused: ${f['reason']}');
        return 0;

      case 'Closed':
        _note('table closed: ${f['reason']}');
        unawaited(closeTable());
        return 0;
    }
    return 0;
  }

  void _played(int player, int card) {
    final v = view;
    if (v == null) return;
    v.played.add((player: player, card: card));
    if (player == playerId) {
      v.myHand.remove(card);
    } else {
      final at = v.opponents.indexWhere((o) => o.player == player);
      if (at >= 0 && v.opponents[at].cards > 0) {
        v.opponents[at] = (player: player, cards: v.opponents[at].cards - 1);
      }
    }
  }

  void _note(String line) {
    log.insert(0, line);
    if (log.length > 40) log.removeLast();
  }

  @override
  void update(double dt) {
    super.update(dt);
    sequencer.pump(dt, applyTableOp);
  }

  @override
  void lifecycleStateChange(AppLifecycleState state) {
    super.lifecycleStateChange(state);
    if (state == AppLifecycleState.resumed) {
      // The lobby's own resume is the mixin's. The table is this game's, and it
      // needs the same treatment for the same reason.
      sequencer.clear();
      unawaited(_table?.resume());
    }
  }

  @override
  void onRemove() {
    unawaited(closeTable());
    super.onRemove();
  }
}
