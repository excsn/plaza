import 'package:flutter/material.dart';
import 'package:plaza_client/plaza_client.dart';

import 'stats.dart';

/// A drop-in connection readout, in the playgrounds' tradition.
///
/// Add it as a Flame overlay. It shows the things that are invisible from
/// inside the game and that every netcode bug turns out to need: whether the
/// two ends agree about the wire format, whether the link is flapping rather
/// than down, and whether frames are arriving that this build cannot read.
///
/// ```dart
/// GameWidget(
///   game: myGame,
///   overlayBuilderMap: {
///     'plaza': (_, MyGame g) => PlazaDebugHud(stats: g.plazaStats, client: g.plaza),
///   },
/// )
/// ```
class PlazaDebugHud extends StatelessWidget {
  const PlazaDebugHud({
    super.key,
    required this.stats,
    this.client,
    this.alignment = Alignment.topRight,
  });

  final PlazaStats stats;
  final PlazaClient? client;
  final Alignment alignment;

  @override
  Widget build(BuildContext context) {
    return Align(
      alignment: alignment,
      child: Padding(
        padding: const EdgeInsets.all(8),
        child: AnimatedBuilder(
          animation: stats,
          builder: (context, _) => _panel(),
        ),
      ),
    );
  }

  Widget _panel() {
    final outdated = stats.outdated;
    final gaveUp = stats.gaveUp;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
      decoration: BoxDecoration(
        color: const Color(0xE61C1F26),
        border: Border.all(color: _statusColour().withValues(alpha: 0.6)),
        borderRadius: BorderRadius.circular(6),
      ),
      child: DefaultTextStyle(
        style: const TextStyle(
          fontFamily: 'monospace',
          fontSize: 11,
          color: Color(0xFFDFE3EA),
          height: 1.4,
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _row('link', stats.status.name, _statusColour()),
            _row('ops in / out', '${stats.opsIn} / ${stats.opsOut}'),
            if (stats.reconnects > 0) _row('reconnects', '${stats.reconnects}'),
            if (stats.resumes > 0) _row('resumes', '${stats.resumes}'),
            // Climbing means the server speaks a kind this build has never
            // heard of, which is additive change working as intended, but the
            // count is how you find out it is happening.
            if (stats.framesSkipped > 0)
              _row('frames skipped', '${stats.framesSkipped}', const Color(0xFFD9A441)),
            if (client?.serverProtocol != null)
              _row('protocol', '${client!.protocol.value} / ${client!.serverProtocol!.value}'),
            if (outdated != null)
              _row('OUTDATED', 'ours ${outdated.ours.value}, theirs ${outdated.theirs.value}',
                  const Color(0xFFE2756A)),
            if (gaveUp != null)
              _row('gave up', 'after ${gaveUp.attempts}', const Color(0xFFE2756A)),
            if (stats.lastDisconnectReason != null)
              _row('last drop', stats.lastDisconnectReason!, const Color(0xFF8B93A3)),
          ],
        ),
      ),
    );
  }

  Color _statusColour() => switch (stats.status) {
        PlazaStatus.open => const Color(0xFF6FCF8F),
        PlazaStatus.connecting || PlazaStatus.reconnecting => const Color(0xFFD9A441),
        PlazaStatus.closed || PlazaStatus.idle => const Color(0xFFE2756A),
      };

  Widget _row(String key, String value, [Color? colour]) => Padding(
        padding: const EdgeInsets.only(bottom: 1),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            SizedBox(
              width: 96,
              child: Text(key, style: const TextStyle(color: Color(0xFF8B93A3))),
            ),
            ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 220),
              child: Text(
                value,
                overflow: TextOverflow.ellipsis,
                style: colour == null ? null : TextStyle(color: colour),
              ),
            ),
          ],
        ),
      );
}
