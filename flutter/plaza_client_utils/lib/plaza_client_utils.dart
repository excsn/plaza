/// Real-time client primitives, ported from `plaza_client_utils` in Rust.
///
/// The whole crate is here, with the Rust unit tests transliterated alongside. A
/// port with nothing to catch the drift is the failure that discipline exists to
/// prevent, so where Dart forced a decision the Rust source did not have to make,
/// the doc comment on the member says so.
///
/// The deterministic network simulator is the one exception, in `net_sim.dart`:
/// the Rust crate gates it behind a `net-sim` feature because it is a test and
/// demo aid, and a separate entry point is how Dart says the same thing.
library;

export 'src/arrival.dart' show ArrivalMonitor;
export 'src/ack.dart' show AckWindow, ackWindow;
export 'src/clock_sync.dart' show ClockSyncEstimator;
export 'src/correction.dart' show Correction, CorrectionMonitor;
export 'src/digest.dart' show SetDigest, mix64;
export 'src/extrapolation.dart' show ExtrapolationBase;
export 'src/error.dart'
    show
        ClientUtilError,
        InputBufferFull,
        InputNotFoundInBuffer,
        InvalidArgument,
        ReconciliationInconsistency;
export 'src/filter.dart' show ScalarKalman;
export 'src/held_input.dart' show HeldInputConfig, HeldInputPredictor;
export 'src/input.dart' show InputCoalescer, TickNamer;
export 'src/interpolation.dart' show InterpolationClock, ServerSnapshot, SnapshotBuffer;
export 'src/math.dart' show Quat, Vec2, Vec3, doubleEpsilon, lerpDouble;
export 'src/smoothing.dart'
    show Easing, ErrorSmoother, easeInCubic, easeInOutQuad, easeInQuad, easeOutCubic, linear, smoothstep;
export 'src/mirror.dart'
    show Agreed, Agreement, DeltaMirror, Divergence, Diverged;
export 'src/playout.dart' show Admission, PlayoutBuffer;
export 'src/predicted_player.dart' show PlayerConfig, PredictedPlayer;
export 'src/prediction.dart' show BufferedInput, ClientInputBuffer, PredictedEntity;
export 'src/remote_view.dart' show RemoteView, RenderOpts;
export 'src/render_timeline.dart' show RenderTimeline;
// The Rust crate re-exports `rollback::Frame`, a `u64` alias for a frame index.
// This does not, because `plaza_wire` exports a `Frame` class of its own for a
// wire frame, and an app importing both packages would not be able to name either.
// A frame index is an `int`; the alias is still there in `src/rollback.dart` for
// anyone who wants it by direct import.
export 'src/rollback.dart'
    show InputTimeline, RollbackConfig, RollbackSession, StateHistory, repeatLastInput;
export 'src/rtt.dart' show RttEstimator;
export 'src/slot.dart' show ReusePolicy, SlotAllocator, SlotKey;
export 'src/timestep.dart' show FixedTimestep, Periodic, Steps, defaultMaxFrameMs;
export 'src/trajectory.dart' show TrajectoryPredictor;
export 'src/types.dart' show ClientTimeMs, SequenceNumber;
export 'src/saturating.dart'
    show
        checkedAdd,
        checkedSub,
        intMax,
        intMin,
        saturatingAdd,
        saturatingMul,
        saturatingSub,
        saturatingSubSigned;
