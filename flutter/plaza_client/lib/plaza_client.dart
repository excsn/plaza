/// The plaza session lifecycle in Dart: handshake, ops, reconnect, resume.
///
/// Transport-agnostic on purpose. Supply a [PlazaSocket] (`web_socket_channel`
/// is the usual answer) and this package stays pure Dart with nothing to
/// conditionally import.
library;

export 'package:plaza_wire/plaza_wire.dart'
    show
        Frame,
        JsonCodec,
        Kind,
        MsgPackCodec,
        ProtocolVersion,
        WireCodec,
        buildFrame,
        splitFrame,
        variant,
        variantBody,
        variantFields,
        variantName;

export 'package:plaza_client_utils/plaza_client_utils.dart'
    show ClockSyncEstimator, RttEstimator;

export 'src/backoff.dart' show Backoff;
export 'src/client.dart'
    show Connected, Disconnected, GaveUp, Outdated, PlazaClient, PlazaEvent, PlazaStatus, SkippedFrame;
export 'src/socket.dart' show LoopbackSocket, PlazaSocket, SocketFactory, SocketState;
export 'src/timeline.dart' show Probe, Timeline;
