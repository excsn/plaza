import 'dart:async';
import 'dart:ui' show AppLifecycleState;

import 'package:flame/game.dart';
import 'package:plaza_flame/plaza_flame.dart';

import 'sequencer.dart';
import 'wire_types.dart';

/// What this client was told it may see, as the scene mutates it.
///
/// Built from the generated [PlayerView], which is decoded straight off the
/// wire; this copy exists because the round is narrated as ops between
/// snapshots and the scene needs somewhere mutable to apply them.
class TableView {
  TableView.fromView(PlayerView v)
      : table = v.table,
        phase = v.phase,
        round = v.round,
        totalRounds = v.totalRounds,
        whoseTurn = v.whoseTurn,
        yourSeat = v.yourSeat,
        coins = v.coins,
        myHand = [for (final c in v.myHand) c.value],
        opponents = [for (final o in v.opponents) (player: o.$1, cards: o.$2)],
        played = [for (final p in v.played) (player: p.$1, card: p.$2.value)],
        scores = [for (final s in v.scores) (player: s.$1, tricks: s.$2)],
        seatsTaken = v.seatsTaken,
        seatsTotal = v.seatsTotal,
        bots = v.bots;

  final String table;
  TablePhase phase;
  int round;
  final int? totalRounds;
  int? whoseTurn;
  final Seat? yourSeat;
  int coins;
  final List<int> myHand;
  final List<({int player, int cards})> opponents;
  final List<({int player, int card})> played;
  final List<({int player, int tricks})> scores;
  final int seatsTaken;
  final int seatsTotal;
  final int bots;

  bool get spectating => yourSeat == Seat.spectator;
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

  // The lobby speaks JSON, so these encode named maps.
  void quickMatch() => sendPlazaOp(const LobbyOpQuickMatch().toWire(named: true));
  void leaveQueue() => sendPlazaOp(const LobbyOpLeaveQueue().toWire(named: true));
  void listTables() => sendPlazaOp(const LobbyOpListTables().toWire(named: true));
  void spectate(String roomId) => sendPlazaOp(LobbyOpSpectate(roomId: roomId).toWire(named: true));

  @override
  void onPlazaOp(Object? op) {
    switch (LobbyOp.fromWire(op)) {
      case LobbyOpWelcome(:final you, coins: final purse):
        playerId = you;
        coins = purse;
        status = 'in the lobby';
      case LobbyOpCatalogue(tables: final listed):
        tables
          ..clear()
          ..addAll(listed);
      case LobbyOpQueued(:final position, :final needed):
        queued = 'place ${position + 1} of $needed';
        status = 'queued';
      case LobbyOpQueueLeft():
        queued = null;
        status = 'in the lobby';
      case LobbyOpPlaced(:final name, :final endpoint, coins: final purse):
        queued = null;
        coins = purse;
        status = 'placed at $name';
        _note('placed at $name');
        unawaited(openTable(Uri.parse(endpoint)));
      case LobbyOpRefused(:final reason):
        queued = null;
        status = 'refused: $reason';
        _note('refused: $reason');
      default:
        break;
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
      // The table speaks compact MessagePack where the lobby speaks JSON: the
      // generated types carry the field order, which is what makes compact safe
      // to read and write here.
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
    final ok = client.sendOps(<Object?>[TableOpPlayCard(Card(rank)).toWire()]);
    if (ok) plazaStats.countOut(1);
    return ok;
  }

  bool get myTurn => view != null && view!.whoseTurn == playerId && view!.phase == TablePhase.playing;

  /// Applies one op and says how long it is worth watching.
  ///
  /// A snapshot arrives on a deal and a resolved trick and nothing in between,
  /// so the rest of the round is narrated as ops and this is the half of that
  /// bargain the client owes.
  double applyTableOp(Object? op) {
    final v = view;
    switch (TableOp.fromWire(op)) {
      case TableOpSnapshot(value: final snapshot):
        view = TableView.fromView(snapshot);
        return 0;

      case TableOpPhaseChanged(value: final notice):
        v?.phase = notice.newPhase;
        return 0;

      case TableOpTurnChanged(value: final notice):
        v?.whoseTurn = notice.newTurnActor;
        return 0;

      case TableOpRoundStarted(value: final notice):
        v?.round = notice.roundNumber;
        return 0;

      case TableOpCardPlayed(:final player, :final card):
      case TableOpPlayedForYou(:final player, :final card):
        _played(player, card.value);
        _note('#$player played ${card.value}');
        return pacing.cardPlayed;

      case TableOpTrickWon(:final player, :final card):
        _note('#$player took the trick with ${card.value}');
        return pacing.trickWon;

      case TableOpSettled(:final winner, coins: final purse):
        coins = purse;
        _note('match over, #$winner takes the stake');
        return pacing.settled;

      case TableOpRejected(:final reason):
        _note('refused: $reason');
        return 0;

      case TableOpClosed(:final reason):
        _note('table closed: $reason');
        unawaited(closeTable());
        return 0;

      default:
        return 0;
    }
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
