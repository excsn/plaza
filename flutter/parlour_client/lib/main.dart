import 'package:flame/game.dart';
import 'package:flutter/material.dart';
import 'package:plaza_flame/plaza_flame.dart';
import 'package:plaza_ws/plaza_ws.dart';

import 'parlour_game.dart';
import 'wire_protocol.dart';

/// A Flame client for `examples/parlour_game`.
///
/// ```sh
/// cargo run -p plaza_example_parlour_game     # in the plaza repo
/// flutter run -d macos                        # here
/// ```
void main() {
  const host = String.fromEnvironment('host', defaultValue: '127.0.0.1:8092');
  runApp(ParlourApp(lobbyUrl: Uri.parse('ws://$host/ws/lobby')));
}

class ParlourApp extends StatefulWidget {
  const ParlourApp({super.key, required this.lobbyUrl});

  final Uri lobbyUrl;

  @override
  State<ParlourApp> createState() => _ParlourAppState();
}

class _ParlourAppState extends State<ParlourApp> {
  late final ParlourGame game = ParlourGame(
    lobbyUrl: widget.lobbyUrl,
    connect: webSocketConnect,
    protocol: const ProtocolVersion(wireProtocol),
  );

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      theme: ThemeData.dark(useMaterial3: true),
      home: Scaffold(
        backgroundColor: const Color(0xFF14161A),
        body: SafeArea(
          child: GameWidget(
            game: game,
            overlayBuilderMap: {
              'table': (_, ParlourGame g) => TableOverlay(game: g),
              'hud': (_, ParlourGame g) => Align(
                    alignment: Alignment.topRight,
                    child: PlazaDebugHud(stats: g.plazaStats),
                  ),
            },
            initialActiveOverlays: const ['table', 'hud'],
          ),
        ),
      ),
    );
  }
}

/// The whole interface. Flame owns the loop and the sequencer; the widget layer
/// only reads what the game already decided, which is why it can be rebuilt on a
/// ticker without coordinating with anything.
class TableOverlay extends StatefulWidget {
  const TableOverlay({super.key, required this.game});

  final ParlourGame game;

  @override
  State<TableOverlay> createState() => _TableOverlayState();
}

class _TableOverlayState extends State<TableOverlay> {
  @override
  void initState() {
    super.initState();
    // The game mutates in its own loop, so the widget layer polls rather than
    // being pushed to. Cheap, and it keeps the game free of Flutter.
    _tick();
  }

  void _tick() async {
    while (mounted) {
      await Future<void>.delayed(const Duration(milliseconds: 80));
      if (mounted) setState(() {});
    }
  }

  @override
  Widget build(BuildContext context) {
    final g = widget.game;
    final v = g.view;
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(children: [
            Text('you #${g.playerId ?? '-'}', style: const TextStyle(color: Color(0xFFD9A441))),
            const SizedBox(width: 16),
            Text('${g.coins} coins', style: const TextStyle(color: Color(0xFFD9A441))),
            const SizedBox(width: 16),
            Text(g.status, style: const TextStyle(color: Color(0xFF8B93A3))),
          ]),
          const SizedBox(height: 12),
          if (!g.seated)
            Row(children: [
              FilledButton(
                onPressed: g.queued == null ? g.quickMatch : g.leaveQueue,
                child: Text(g.queued == null ? 'Quick match' : 'Leave queue (${g.queued})'),
              ),
              const SizedBox(width: 12),
              OutlinedButton(onPressed: g.listTables, child: const Text('Refresh')),
            ]),
          if (v != null) ...[
            Text('${v.table}  ${v.phase.wireName}  round ${v.round}/${v.totalRounds ?? '-'}'
                '  ${v.seatsTaken}/${v.seatsTotal} seats, ${v.bots} bots'),
            const SizedBox(height: 10),
            const Text('played', style: TextStyle(color: Color(0xFF8B93A3), fontSize: 12)),
            _Cards(ranks: v.played.map((p) => p.card).toList(), faceUp: true),
            const SizedBox(height: 10),
            Text(v.spectating ? 'watching' : 'your hand',
                style: const TextStyle(color: Color(0xFF8B93A3), fontSize: 12)),
            _Cards(
              ranks: v.myHand,
              faceUp: true,
              onTap: g.myTurn ? g.playCard : null,
            ),
            const SizedBox(height: 10),
            for (final o in v.opponents) ...[
              Text('player #${o.player}', style: const TextStyle(color: Color(0xFF8B93A3), fontSize: 12)),
              // Backs, because the ranks were never in this client's frame.
              _Cards(ranks: List<int>.filled(o.cards, 0), faceUp: false),
            ],
          ],
          const Spacer(),
          SizedBox(
            height: 90,
            child: ListView(
              children: [
                for (final line in g.log)
                  Text(line, style: const TextStyle(color: Color(0xFF8B93A3), fontSize: 12)),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _Cards extends StatelessWidget {
  const _Cards({required this.ranks, required this.faceUp, this.onTap});

  final List<int> ranks;
  final bool faceUp;
  final void Function(int rank)? onTap;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 62,
      child: Row(
        children: [
          for (final rank in ranks)
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: GestureDetector(
                onTap: onTap == null ? null : () => onTap!(rank),
                child: Container(
                  width: 42,
                  height: 60,
                  decoration: BoxDecoration(
                    color: faceUp ? const Color(0xFF232833) : const Color(0xFF2F3541),
                    border: Border.all(color: onTap == null ? const Color(0xFF2C313B) : const Color(0xFF6AA9E0)),
                    borderRadius: BorderRadius.circular(5),
                  ),
                  alignment: Alignment.center,
                  child: Text(
                    faceUp ? '$rank' : '?',
                    style: TextStyle(
                      fontSize: faceUp ? 18 : 13,
                      fontWeight: FontWeight.bold,
                      color: faceUp ? const Color(0xFFDFE3EA) : const Color(0xFF8B93A3),
                    ),
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }
}
