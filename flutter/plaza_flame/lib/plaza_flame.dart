/// Flame glue for plaza: a game mixin that owns the connection, and a readout.
///
/// Deliberately thin. Anything thicker than wiring belongs in the game or in a
/// utils package.
library;

export 'package:plaza_client/plaza_client.dart';

export 'package:plaza_client_utils/plaza_client_utils.dart';

export 'src/game.dart' show PlazaGame;
export 'src/hud.dart' show PlazaDebugHud;
export 'src/stats.dart' show PlazaStats;
