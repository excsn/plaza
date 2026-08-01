/// A console client for `examples/lobby_world`, and the worked answer to "what
/// does an app actually do about a version skew".
///
/// The handshake is reported, never enforced: plaza records what a peer declared
/// and keeps serving it, because a version is a build hash and a peer that merely
/// recompiled is indistinguishable from one whose shapes changed. So the policy is
/// the application's, and this file is one. It is deliberately a *harsh* one, and
/// says why below.
///
/// Run it against a live server:
///
///   cd examples && cargo run -p plaza_example_lobby_world &
///   cd flutter/plaza_ws && dart run example/lobby_client.dart
///
/// And against a version the server cannot agree with:
///
///   dart run example/lobby_client.dart --protocol 1
///
/// Exit codes are what `../e2e.sh` asserts: 0 played, 2 refused on skew, 1 never
/// got there.
library;

import 'dart:async';
import 'dart:io';

import 'package:plaza_ws/plaza_ws.dart';

const _skewExit = 2;
const _failedExit = 1;

Future<void> main(List<String> args) async {
  final url = _option(args, '--url') ?? 'ws://127.0.0.1:8090/ws/lobby';
  final seconds = int.parse(_option(args, '--seconds') ?? '3');

  // A real app has this from the const its `build.rs` publishes with
  // `plaza_wire::build::emit`. Passing it in is what lets this demonstrate both
  // outcomes against one server.
  final declared = ProtocolVersion(int.parse(_option(args, '--protocol') ?? '0'));

  final client = PlazaClient(
    url: Uri.parse(url),
    connect: webSocketConnect,
    protocol: declared,
    backoff: Backoff(initial: const Duration(milliseconds: 250), maxAttempts: 3),
  );

  final done = Completer<int>();
  void finish(int code) {
    if (!done.isCompleted) done.complete(code);
  }

  client.events.listen((event) {
    switch (event) {
      case Connected(resumed: final resumed):
        stdout.writeln(resumed ? '· reconnected' : '· connected to $url');

      // The whole point of the file. Plaza has told us the server speaks
      // something else and has left the connection open; what happens next is
      // ours to choose, and there are at least four defensible answers:
      //
      //   - stop, and tell the user to update. What this does, because a console
      //     client cannot reload itself and playing on would corrupt state that
      //     someone else can see.
      //   - keep playing read-only: render what decodes, send nothing.
      //   - keep playing anyway, if the mismatch is a recompile you trust.
      //   - reload, which is what a browser client does and an installed app
      //     cannot.
      //
      // Retrying is the one answer that is always wrong: the next connection
      // reaches the same server with the same two versions.
      case Outdated(ours: final ours, theirs: final theirs):
        stderr.writeln('! this build speaks ${ours.value}, the server speaks ${theirs.value}');
        stderr.writeln('! update and reconnect; retrying would reach the same server');
        finish(_skewExit);

      case Disconnected(reason: final reason):
        stdout.writeln('· disconnected: $reason');

      case GaveUp(attempts: final attempts):
        stderr.writeln('! gave up after $attempts attempts');
        finish(_failedExit);

      // A kind this build has never heard of. Skipped, not fatal, which is what
      // lets a newer server add frame kinds without breaking this client.
      case SkippedFrame(kindByte: final kind):
        stdout.writeln('· skipped a frame of kind $kind');
    }
  });

  client.ops.listen((op) {
    // `variantName` and `variantFields` rather than `op['Welcome']`, and this is
    // the trap they exist for: serde writes a struct variant as a one-entry map
    // but a *unit* variant as a bare string, so `QueueLeft` below arrives as
    // `"QueueLeft"`. A client that only ever indexes drops it silently, and the
    // symptom is indistinguishable from the server not sending.
    final name = variantName(op);
    final fields = variantFields(op);

    switch (name) {
      case 'Welcome':
        final link = fields['link'] as Map<String, Object?>;
        stdout.writeln('  you are player ${fields['you']}, '
            '${fields['coins']} coins, ${link['one_way_ms']}ms one way');
      case 'Catalogue':
        final rooms = (fields['rooms'] as List<Object?>).cast<Map<String, Object?>>();
        stdout.writeln('  ${rooms.length} arenas:');
        for (final room in rooms) {
          final budget = room['budget_ms'] ?? 'unlimited';
          stdout.writeln('    ${room['name']}: '
              '${room['current_players']}/${room['max_players']} seats, '
              'budget $budget, ${room['playable'] == true ? 'playable' : 'too slow for this link'}');
        }
      case 'Queued':
        stdout.writeln('  queued at ${fields['position']}, needs ${fields['needed']}, '
            'bots fill in ${fields['patience_ms']}ms');
      case 'QueueLeft':
        stdout.writeln('  left the queue (a unit variant: arrived as a bare string)');
      case 'Placed':
        stdout.writeln('  placed in ${fields['name']} at ${fields['endpoint']}'
            '${fields['spectator'] == true ? ' as a spectator' : ''}');
      case 'Refused':
        stdout.writeln('  refused: ${fields['reason']}');
      default:
        stdout.writeln('  $name');
    }
  });

  await client.start();
  if (client.status != PlazaStatus.open) {
    stderr.writeln('! could not reach $url');
    exitCode = _failedExit;
    return;
  }

  // The server speaks first, so give its Welcome and Catalogue a moment before
  // asking for anything.
  await Future<void>.delayed(const Duration(milliseconds: 300));

  if (client.serverProtocol != null) {
    stdout.writeln('· server speaks ${client.serverProtocol!.value}, '
        'this build ${declared.value == 0 ? 'declares nothing' : 'speaks ${declared.value}'}');
  }

  if (!done.isCompleted) {
    client.sendOp(variant('ListRooms'));
    await Future<void>.delayed(const Duration(milliseconds: 200));
    client.sendOp(variant('QuickMatch'));
    await Future<void>.delayed(const Duration(milliseconds: 200));
    client.sendOp(variant('LeaveQueue'));
  }

  // Whichever comes first: the skew handler deciding, or the run finishing.
  final code = await Future.any<int>([
    done.future,
    Future<int>.delayed(Duration(seconds: seconds), () => 0),
  ]);

  await client.stop();

  // Assigned rather than returned: the VM ignores what `main` returns, and
  // `exit()` here would cut the socket close short.
  exitCode = code;
}

String? _option(List<String> args, String name) {
  final i = args.indexOf(name);
  return i >= 0 && i + 1 < args.length ? args[i + 1] : null;
}
