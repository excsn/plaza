import 'dart:convert';
import 'dart:io';

import 'package:plaza_client_utils/net_sim.dart';
import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Golden behaviour vectors written by the Rust side.
///
/// The transliterated tests in this package catch a *porting* mistake. They cannot
/// catch a later change in Rust: rewrite the extrapolation cap or the playout
/// admission rule there and every one of them still passes while the two languages
/// quietly disagree. This replays scenarios the Rust crate scripted and compares
/// the outputs, so a behaviour change fails in `cargo test` first and then here
/// until the port is brought along.
///
/// Regenerate with:
///
///   PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_client_utils \
///     --features net-sim --test dart_vectors
const fixtures = '../fixtures';

Map<String, dynamic> load(String name) =>
    jsonDecode(File('$fixtures/$name.json').readAsStringSync()) as Map<String, dynamic>;

double d(Object? v) => (v as num).toDouble();
int i(Object? v) => (v as num).toInt();
List<Object?> list(Object? v) => v as List<Object?>;
Map<String, dynamic> map(Object? v) => v as Map<String, dynamic>;

/// What a file promises about its floats.
///
/// Rust computes these in `f32` and Dart has only `double`, so the last bits
/// differ by construction. Absolute alone does not scale: f32 spacing at 10,000 is
/// about 1e-3, so a fixed 1e-4 would reject a correct value the moment a scenario
/// used world coordinates. Relative alone breaks at the other end, where a value
/// that should settle at zero fails any relative test. So either passing is enough.
class Tol {
  const Tol(this.abs, this.rel);

  factory Tol.of(Map<String, dynamic> file) =>
      Tol(d(file['tolerance']), d(file['rel_tolerance']));

  final double abs;
  final double rel;

  bool accepts(double want, double got) {
    final delta = (want - got).abs();
    if (delta <= abs) return true;
    final scale = want.abs() > got.abs() ? want.abs() : got.abs();
    return delta <= rel * scale;
  }
}

Matcher near(Object? expected, Tol tol) {
  if (expected == null) return isNull;
  final want = d(expected);
  return predicate<Object?>(
    (actual) => actual is num && tol.accepts(want, actual.toDouble()),
    'within ${tol.abs} absolute or ${tol.rel} relative of $want',
  );
}

void main() {
  setUpAll(() {
    if (!Directory(fixtures).existsSync()) {
      fail('fixtures missing: run the Rust generator (see this file\'s docs)');
    }
  });

  group('estimators', () {
    final v = load('vectors_estimators');
    final tol = Tol.of(v);

    test('RttEstimator tracks the Rust smoothing step for step', () {
      final section = map(v['rtt_estimator']);
      final rtt = RttEstimator(d(section['alpha']));
      for (final step in list(section['steps'])) {
        final s = map(step);
        rtt.observe(i(s['sample_ms']));
        final at = 'after sample ${s['sample_ms']}';
        expect(rtt.rttMs, near(s['rtt_ms'], tol), reason: 'rtt $at');
        expect(rtt.oneWayMs, near(s['one_way_ms'], tol), reason: 'one way $at');
        expect(rtt.minRttMs, near(s['min_rtt_ms'], tol), reason: 'min $at');
        expect(rtt.jitterMs, near(s['jitter_ms'], tol), reason: 'jitter $at');
      }
    });

    /// Rust saturates; Dart has no `saturating_sub` and had to clamp. A negative
    /// round trip poisons a smoothed average for minutes.
    test('a pong stamped after its own arrival clamps rather than going negative', () {
      final section = map(v['rtt_pong_stamped_after_arrival']);
      final rtt = RttEstimator(0.2)
        ..observePong(i(section['origin_ms']), i(section['now_ms']));
      expect(rtt.rttMs, near(section['rtt_ms'], tol));
    });

    test('ClockSyncEstimator fits the same line', () {
      final section = map(v['clock_sync']);
      final clock = ClockSyncEstimator(i(section['window']));
      for (final step in list(section['steps'])) {
        final s = map(step);
        clock.observe(d(s['local_ms']), d(s['offset_ms']));
        final at = 'at local ${s['local_ms']}';
        expect(clock.isReady, s['ready'], reason: 'ready $at');
        expect(clock.sampleCount, i(s['samples']), reason: 'samples $at');
        expect(clock.skew, near(s['skew'], tol), reason: 'skew $at');
        expect(clock.offsetAt(d(s['local_ms'])), near(s['offset_at_local'], tol), reason: 'offset $at');
        expect(
          clock.serverTimeAt(d(s['local_ms'])),
          near(s['server_time_at_local'], tol),
          reason: 'server time $at',
        );
      }
    });

    test('ArrivalMonitor measures the same stream', () {
      final section = map(v['arrival_monitor']);
      final arrival = ArrivalMonitor(d(section['smoothing']));
      for (final step in list(section['steps'])) {
        final s = map(step);
        arrival.observe(i(s['stamp']), i(s['recv']));
        final at = 'at stamp ${s['stamp']}';
        expect(arrival.intervalMs, near(s['interval_ms'], tol), reason: 'interval $at');
        expect(arrival.latenessMs, near(s['lateness_ms'], tol), reason: 'lateness $at');
        expect(arrival.jitterMs, near(s['jitter_ms'], tol), reason: 'jitter $at');
        expect(arrival.neededDelayMs, near(s['needed_delay_ms'], tol), reason: 'needed $at');
        expect(arrival.warmedUp, s['warmed_up'], reason: 'warmed up $at');
      }
    });

    test('ScalarKalman converges identically', () {
      final section = map(v['scalar_kalman']);
      final filter = ScalarKalman(d(section['process_noise']), d(section['measurement_noise']));
      for (final step in list(section['steps'])) {
        final s = map(step);
        final returned = filter.observe(d(s['measurement']));
        final at = 'after ${s['measurement']}';
        expect(returned, near(s['returned'], tol), reason: 'returned $at');
        expect(filter.estimate, near(s['estimate'], tol), reason: 'estimate $at');
        expect(filter.variance, near(s['variance'], tol), reason: 'variance $at');
        expect(filter.lastGain, near(s['last_gain'], tol), reason: 'gain $at');
      }
    });

    /// The winsorising rule, which is the whole reason this type exists: one
    /// respawn-sized correction must not lift the baseline so far that the next
    /// real problem hides under it.
    test('CorrectionMonitor flags the same samples', () {
      final section = map(v['correction_monitor']);
      final monitor = CorrectionMonitor(floor: d(section['floor']), warmup: i(section['warmup']));
      final magnitudes = <double>[
        for (var k = 0; k < 300; k++) 10.0 + (k % 5) * 0.2,
        5000.0,
        12.0,
        500.0,
        11.0,
      ];
      final flagged = <int>[];
      final tail = <Map<String, Object?>>[];
      for (var k = 0; k < magnitudes.length; k++) {
        final wasFlagged = monitor.record(magnitudes[k]);
        if (wasFlagged) flagged.add(k);
        if (k >= 296) {
          tail.add({
            'index': k,
            'flagged': wasFlagged,
            'norm': monitor.norm,
            'threshold': monitor.threshold,
            'band': monitor.band,
            'peak': monitor.peak,
          });
        }
      }

      expect(flagged, list(section['flagged_indices']).map(i).toList());
      final expectedTail = list(section['tail']);
      expect(tail.length, expectedTail.length);
      for (var k = 0; k < tail.length; k++) {
        final want = map(expectedTail[k]);
        final at = 'index ${want['index']}';
        expect(tail[k]['index'], i(want['index']), reason: at);
        expect(tail[k]['flagged'], want['flagged'], reason: 'flagged at $at');
        expect(tail[k]['norm'], near(want['norm'], tol), reason: 'norm at $at');
        expect(tail[k]['threshold'], near(want['threshold'], tol), reason: 'threshold at $at');
        expect(tail[k]['band'], near(want['band'], tol), reason: 'band at $at');
        expect(tail[k]['peak'], near(want['peak'], tol), reason: 'peak at $at');
      }
      expect(monitor.counts, (i(section['samples']), i(section['outliers'])));
    });
  });

  group('timing', () {
    final v = load('vectors_timing');
    final tol = Tol.of(v);

    test('InterpolationClock lands on the same target', () {
      final section = map(v['interpolation_clock']);
      final clock = InterpolationClock(i(section['delay_ms']));
      for (final step in list(section['steps'])) {
        final s = map(step);
        switch (s['op']) {
          case 'observe':
            clock.observe(i(s['arg']));
          case 'advance':
            clock.advance(i(s['arg']));
          case 'resync':
            clock.resync(i(s['arg']), d(s['strength']));
          case 'set_delay':
            clock.delay = i(s['arg']);
          default:
            fail('unknown op ${s['op']}');
        }
        expect(clock.target, s['target'] == null ? isNull : i(s['target']),
            reason: '${s['op']} ${s['arg']}');
      }
    });

    test('SnapshotBuffer interpolates and clamps the same way', () {
      final section = map(v['snapshot_buffer']);
      final buffer = SnapshotBuffer<double>(
        maxSize: i(section['max_size']),
        lerp: (a, b, t) => a + (b - a) * t,
      );
      for (final pair in list(section['snapshots'])) {
        final p = list(pair);
        buffer.add(i(p[0]), d(p[1]));
      }
      for (final query in list(section['queries'])) {
        final q = map(query);
        expect(buffer.at(i(q['target'])), near(q['state'], tol), reason: 'at ${q['target']}');
      }
    });

    /// The port first held the raw sample past the limit, which is a jump of the
    /// whole extrapolation window in the wrong direction. This is the vector that
    /// would have caught it.
    test('ExtrapolationBase caps the duration rather than discarding it', () {
      final section = map(v['extrapolation_base']);
      final base = ExtrapolationBase<Vec3, Vec3>(
        state: Vec3(d(section['state_x']), 0.0, 0.0),
        velocity: Vec3(d(section['velocity_x']), 0.0, 0.0),
        serverTimestamp: 0,
        clientReceiptTimeMs: 0,
        extrapolateBy: (s, vel, dt) => s.extrapolate(vel, dt),
      );
      final cap = i(section['max_extrapolation_ms']);
      for (final query in list(section['queries'])) {
        final q = map(query);
        expect(base.at(i(q['target_ms']), cap).x, near(q['x'], tol), reason: 'at ${q['target_ms']}ms');
      }
      expect(base.overExtrapolations, i(section['over_extrapolations']),
          reason: 'holding at the cap is a rate to watch, not an error');
    });

    test('TrajectoryPredictor fits the same curve', () {
      final section = map(v['trajectory']);
      final p = TrajectoryPredictor(
        damping: d(section['damping']),
        maxHorizonMs: i(section['max_horizon_ms']),
      );
      for (final step in list(section['steps'])) {
        final s = map(step);
        p.observe(i(s['observed_ms']), d(s['value']));
        final at = 'after ${s['observed_ms']}ms';
        expect(p.samples, i(s['samples']), reason: 'samples $at');
        expect(p.velocity, near(s['velocity'], tol), reason: 'velocity $at');
        expect(p.acceleration, near(s['acceleration'], tol), reason: 'acceleration $at');
        expect(p.predict(400), near(s['predict_at_400'], tol), reason: 'predict 400 $at');
        expect(p.predict(1000), near(s['predict_at_1000'], tol), reason: 'predict 1000 $at');
        expect(p.predict(150), near(s['predict_at_150'], tol), reason: 'predict 150 $at');
      }
    });

    test('FixedTimestep paces and drops identically', () {
      final section = map(v['fixed_timestep']);
      final timestep = FixedTimestep.fromStepMs(
        i(section['step_ms']),
        maxFrameMs: i(section['max_frame_ms']),
      );
      for (final step in list(section['steps'])) {
        final s = map(step);
        final steps = timestep.advance(i(s['elapsed_ms']));
        final at = 'after ${s['elapsed_ms']}ms';
        expect(steps.length, i(s['steps']), reason: 'count $at');
        expect(steps.toList(), list(s['step_nanos']).map(i).toList(), reason: 'values $at');
        expect(timestep.pendingMs, i(s['pending_ms']), reason: 'pending $at');
        expect(timestep.alpha, near(s['alpha'], tol), reason: 'alpha $at');
        expect(timestep.droppedMs, i(s['dropped_ms']), reason: 'dropped $at');
      }
    });

    /// The cross-language pin for a rate that does not divide a round number:
    /// the step in nanoseconds must be the one Rust computes, and the counts
    /// walk 5,6,6,... to 59 over the second, which only a sub-millisecond step
    /// produces.
    test('FixedTimestep.fromHz means the same rate as the Rust driver', () {
      final section = map(v['fixed_timestep_hz']);
      final timestep = FixedTimestep.fromHz(i(section['hz']));
      expect(timestep.stepNanos, i(section['step_nanos']));
      for (final step in list(section['steps'])) {
        final s = map(step);
        expect(timestep.advance(i(s['elapsed_ms'])).length, i(s['steps']),
            reason: 'after ${s['elapsed_ms']}ms');
      }
    });

    test('Periodic fires the same number of times', () {
      final section = map(v['periodic']);
      final periodic = Periodic(i(section['interval_ms']));
      for (final step in list(section['steps'])) {
        final s = map(step);
        expect(periodic.advance(i(s['elapsed_ms'])), i(s['fired']),
            reason: 'after ${s['elapsed_ms']}ms');
      }
    });

    test('Periodic.fromHz means the same rate as the Rust side', () {
      final section = map(v['periodic_hz']);
      final periodic = Periodic.fromHz(i(section['hz']));
      expect(periodic.intervalNanos, i(section['interval_nanos']));
      for (final step in list(section['steps'])) {
        final s = map(step);
        expect(periodic.advance(i(s['elapsed_ms'])), i(s['fired']),
            reason: 'after ${s['elapsed_ms']}ms');
      }
    });

    /// A gap wider than `lostAhead` is a discontinuity and not a delay, and the
    /// caller has to hear about it or its entity mirror is wrong for good.
    test('PlayoutBuffer admits, drops and restarts identically', () {
      final section = map(v['playout']);
      final playout = PlayoutBuffer<int>(
        maxQueued: i(section['max_queued']),
        lostAhead: i(section['lost_ahead']),
      );
      for (final arrival in list(section['arrivals'])) {
        final a = map(arrival);
        final admission = playout.push(
          i(a['stamp']),
          i(a['order']),
          i(a['order']),
          a['render_at'] == null ? null : i(a['render_at']),
        );
        final at = 'order ${a['order']}';
        final name = admission == Admission.queued ? 'Queued' : 'TimelineLost';
        expect(name, a['admission'], reason: 'admission for $at');
        expect(playout.length, i(a['len']), reason: 'len after $at');
        expect(playout.restarts, i(a['restarts']), reason: 'restarts after $at');
        expect(playout.underruns, i(a['underruns']), reason: 'underruns after $at');
      }
      for (final pop in list(section['pops'])) {
        final p = map(pop);
        expect(playout.popDue(i(p['render_at'])), p['popped'] == null ? isNull : i(p['popped']),
            reason: 'pop at ${p['render_at']}');
      }
    });

    test('the easing curves have the same shape', () {
      final section = map(v['easing']);
      final inputs = list(section['inputs']).map(d).toList();
      final curves = map(section['curves']);
      final byName = <String, Easing>{
        'linear': linear,
        'smoothstep': smoothstep,
        'ease_out_cubic': easeOutCubic,
        'ease_in_cubic': easeInCubic,
        'ease_in_quad': easeInQuad,
        'ease_in_out_quad': easeInOutQuad,
      };
      for (final entry in byName.entries) {
        final expected = list(curves[entry.key]!).map(d).toList();
        for (var k = 0; k < inputs.length; k++) {
          expect(entry.value(inputs[k]), near(expected[k], tol),
              reason: '${entry.key} at t=${inputs[k]}');
        }
      }
    });

    test('ErrorSmoother eases along the same path', () {
      final section = map(v['error_smoother']);
      final smoother = ErrorSmoother<double>(d(section['duration_secs']), easing: smoothstep)
        ..beginFrom(d(section['begin_from']));
      for (final step in list(section['steps'])) {
        final s = map(step);
        smoother.advance(d(s['dt_secs']));
        final sampled = smoother.sample(0.0, (a, b, t) => a + (b - a) * t);
        expect(sampled, near(s['sample_at_logical_0'], tol), reason: 'after ${s['dt_secs']}s');
        expect(smoother.isEasing, s['is_easing'], reason: 'easing after ${s['dt_secs']}s');
      }
    });
  });

  /// Everything here is compared exactly. A digest or a slot key that disagrees is
  /// unrecoverable: both sides would blame the world for a bug that was only ever
  /// in the arithmetic, and the recovery machinery would fire for ever.
  group('bookkeeping', () {
    final v = load('vectors_bookkeeping');

    test('AckWindow tracks the same mask', () {
      final section = map(v['ack_window']);
      final acks = AckWindow();
      for (final step in list(section['steps'])) {
        final s = map(step);
        final seq = i(s['seq']);
        expect(acks.observe(seq), s['fresh'], reason: 'fresh for $seq');
        expect(acks.newest, s['newest'] == null ? isNull : i(s['newest']), reason: 'newest at $seq');
        expect(acks.mask, i(s['mask']), reason: 'mask at $seq');
        expect(acks.receivedInWindow, i(s['received_in_window']), reason: 'count at $seq');
        expect(acks.contains(seq), s['contains_seq'], reason: 'contains $seq');
        final encoded = acks.encode();
        if (s['encoded'] == null) {
          expect(encoded, isNull, reason: 'encoded at $seq');
        } else {
          final want = list(s['encoded']);
          expect(encoded!.$1, i(want[0]), reason: 'encoded newest at $seq');
          expect(encoded.$2, i(want[1]), reason: 'encoded mask at $seq');
        }
      }
      expect(acks.missingSince(150).toList(), list(section['missing_since_150']).map(i).toList());
    });

    /// The bit packing is the contract: `SetDigest` and the delta baselines are
    /// keyed on the encoded value, so a different layout is a different key space.
    test('SlotKey encodes to the same integers', () {
      for (final entry in list(map(v['slot_key'])['encodings'])) {
        final e = map(entry);
        final key = SlotKey(i(e['index']), i(e['generation']));
        final at = 'key ${e['index']}/${e['generation']}';
        expect(key.encode(), i(e['encoded']), reason: at);
        final round = SlotKey.decode(i(e['encoded']));
        expect(round.index, i(e['decoded_index']), reason: 'index of $at');
        expect(round.generation, i(e['decoded_generation']), reason: 'generation of $at');
        expect(key.ungenerational().encode(), i(e['ungenerational_encoded']), reason: 'stripped $at');
      }
    });

    test('SlotAllocator recycles in the same order under each policy', () {
      for (final entry in list(v['slot_allocator'])) {
        final section = map(entry);
        final policy = section['policy'] == 'Lifo' ? ReusePolicy.lifo : ReusePolicy.fifo;
        final allocator = SlotAllocator(policy: policy);
        final held = <SlotKey>[];
        for (final event in list(section['events'])) {
          final e = map(event);
          final at = '${section['policy']} ${e['op']}';
          switch (e['op']) {
            case 'alloc':
              final key = allocator.alloc();
              held.add(key);
              expect(key.encode(), i(e['encoded']), reason: at);
              expect(allocator.length, i(e['len']), reason: 'len after $at');
            case 'free':
              final key = SlotKey.decode(i(e['encoded']));
              expect(allocator.free(key), e['freed'], reason: at);
              expect(allocator.length, i(e['len']), reason: 'len after $at');
            case 'is_live_stale':
              final key = SlotKey.decode(i(e['encoded']));
              expect(allocator.isLive(key), e['live'], reason: at);
            case 'index_space':
              expect(allocator.indexSpace, i(e['value']), reason: at);
            default:
              fail('unknown op ${e['op']}');
          }
        }
      }
    });

    test('DeltaMirror computes the same digest', () {
      final mirror = DeltaMirror<int>();
      final steps = list(map(v['delta_mirror'])['steps']);
      for (final step in steps) {
        final s = map(step);
        switch (s['op']) {
          case 'baseline':
            mirror.begin(i(s['seq']), fullBaseline: true);
            for (final pair in [(0, 1), (5, 1), (9, 3)]) {
              mirror.insert(SlotKey(pair.$1, pair.$2), pair.$1);
            }
            expect(mirror.digest, i(s['digest']), reason: 'baseline digest');
            expect(mirror.length, i(s['len']), reason: 'baseline len');
          case 'delta':
            mirror.begin(i(s['seq']), fullBaseline: false);
            mirror.insert(const SlotKey(12, 1), 12);
            mirror.remove(const SlotKey(5, 1));
            expect(mirror.digest, i(s['digest']), reason: 'delta digest');
            expect(mirror.length, i(s['len']), reason: 'delta len');
          case 'settle_matching':
            expect(mirror.settle(mirror.digest).agreed, s['agreed'], reason: 'matching settle');
          case 'settle_mismatched':
            expect(mirror.settle(0).agreed, s['agreed'], reason: 'mismatched settle');
          case 'divergence_from':
            final serverKeys = list(s['server_keys']).map(i).toList();
            final divergence = mirror.divergenceFrom(serverKeys);
            expect(divergence.extra.map((k) => k.encode()).toList(),
                list(s['extra']).map(i).toList(), reason: 'extra');
            expect(divergence.missing.map((k) => k.encode()).toList(),
                list(s['missing']).map(i).toList(), reason: 'missing');
          default:
            fail('unknown op ${s['op']}');
        }
      }
    });

    test('InputCoalescer suppresses and keeps alive at the same instants', () {
      final section = map(v['input_coalescer']);
      final coalescer = InputCoalescer<int>(i(section['keepalive_ms']));
      for (final step in list(section['steps'])) {
        final s = map(step);
        expect(coalescer.shouldSend(i(s['input']), i(s['now_ms'])), s['should_send'],
            reason: 'input ${s['input']} at ${s['now_ms']}ms');
      }
    });
  });

  group('prediction', () {
    final v = load('vectors_prediction');
    final tol = Tol.of(v);

    test('PredictedEntity replays to the same state', () {
      final section = map(v['predicted_entity']);
      final entity = PredictedEntity<double, double>(0.0);
      final buffer = ClientInputBuffer<double, double>(i(section['input_buffer']));
      double apply(double state, double op) => state + op;

      for (final step in list(section['steps'])) {
        final s = map(step);
        if (s['op'] == 'input') {
          entity.applyLocal(d(s['input']), i(s['seq']), buffer, apply);
          expect(entity.predicted, near(s['predicted'], tol), reason: 'after seq ${s['seq']}');
          expect(buffer.length, i(s['buffered']), reason: 'buffered after seq ${s['seq']}');
        } else {
          entity.reconcile(d(s['authoritative']), i(s['acked_seq']), buffer, apply);
          expect(entity.predicted, near(s['predicted'], tol), reason: 'reconciled');
          expect(entity.authoritative, near(s['last_authoritative'], tol), reason: 'authoritative');
          expect(buffer.length, i(s['buffered']), reason: 'buffered after reconcile');
        }
      }
    });

    /// Past this point a reconciliation cannot replay everything the server has not
    /// acknowledged, so the prediction is wrong by whatever the dropped inputs did.
    /// The count is the difference between a prediction that is late and one that is
    /// wrong, which is why it is a number and not only a log line.
    test('an overflowing input buffer drops and counts identically', () {
      final section = map(v['input_buffer_overflow']);
      final buffer = ClientInputBuffer<double, double>(i(section['max_size']));
      for (var seq = 1; seq <= i(section['recorded']); seq++) {
        buffer.record(seq, seq.toDouble(), 0.0);
      }
      expect(buffer.length, i(section['len']));
      expect(buffer.overflowed, i(section['overflowed']));
      expect(buffer.unacknowledgedAfter(0).first.sequenceNumber,
          i(section['oldest_retained_seq']));
    });

    /// The logical state is what game rules read and the render state is what the
    /// eye sees. Conflating them is the bug the split exists to prevent, so both
    /// are pinned at every step.
    test('PredictedPlayer separates the logical and render paths identically', () {
      final section = map(v['predicted_player']);
      final player = PredictedPlayer<double, double, void>(
        initial: 0.0,
        config: PlayerConfig(
          inputBuffer: i(section['input_buffer']),
          smoothingSecs: d(section['smoothing_secs']),
        ),
        apply: (state, input, _) => state + input,
        lerp: (a, b, t) => a + (b - a) * t,
        context: null,
      );
      for (final step in list(section['steps'])) {
        final s = map(step);
        switch (s['op']) {
          case 'input':
            final seq = player.input(2.0);
            expect(seq, i(s['seq']), reason: 'sequence number');
            expect(player.logical, near(s['logical'], tol), reason: 'logical at seq $seq');
            expect(player.render(), near(s['render'], tol), reason: 'render at seq $seq');
            expect(player.unackedCount, i(s['unacked']), reason: 'unacked at seq $seq');
          case 'reconcile':
            final correction = player.reconcile(d(s['authoritative']), i(s['acked_seq']));
            expect(correction.seen, near(s['seen'], tol), reason: 'seen');
            expect(correction.settled, near(s['settled'], tol), reason: 'settled');
            expect(player.logical, near(s['logical'], tol), reason: 'logical after reconcile');
            expect(player.render(), near(s['render'], tol), reason: 'render after reconcile');
            expect(player.unackedCount, i(s['unacked']), reason: 'unacked after reconcile');
          case 'advance':
            player.advance(d(s['dt_secs']));
            expect(player.logical, near(s['logical'], tol), reason: 'logical while easing');
            expect(player.render(), near(s['render'], tol), reason: 'render while easing');
          default:
            fail('unknown op ${s['op']}');
        }
      }
    });

    test('HeldInputPredictor dead reckons and corrects identically', () {
      final section = map(v['held_input']);
      final held = HeldInputPredictor<double, double, void>(
        initial: 0.0,
        initialInput: 0.0,
        config: HeldInputConfig(blend: d(section['blend'])),
        integrate: (state, input, dt, _) => state + input * dt,
        lerp: (a, b, t) => a + (b - a) * t,
        context: null,
      )..hold(d(section['hold']));

      for (final step in list(section['steps'])) {
        final s = map(step);
        held.advance(1.0 / 60.0);
        if (s['op'] == 'reconcile') {
          final correction = held.reconcile(d(s['authoritative']), d(s['age_secs']));
          expect(correction.seen, near(s['seen'], tol), reason: 'seen at ${s['authoritative']}');
          expect(correction.settled, near(s['settled'], tol), reason: 'settled at ${s['authoritative']}');
        } else {
          expect(held.logical, near(s['logical'], tol), reason: 'dead reckoned');
        }
      }
    });

    test('RemoteView interpolates, dead reckons and holds identically', () {
      final section = map(v['remote_view']);
      final view = RemoteView<Vec3, Vec3>(
        bufferSize: i(section['buffer_size']),
        maxExtrapolationMs: i(section['max_extrapolation_ms']),
        lerp: (a, b, t) => a.lerp(b, t),
        extrapolateBy: (s, vel, dt) => s.extrapolate(vel, dt),
      );
      final velocity = Vec3(d(section['velocity_x']), 0.0, 0.0);
      for (final sample in list(section['samples'])) {
        final s = list(sample);
        view.push(i(s[0]), Vec3(d(s[1]), 0.0, 0.0), velocity);
      }
      for (final query in list(section['queries'])) {
        final q = map(query);
        final at = 'at ${q['target_ms']}ms';
        final target = i(q['target_ms']);
        expect(view.render(target)?.x, near(q['interpolated_x'], tol), reason: 'interpolated $at');
        expect(
          view.render(target, const RenderOpts(extrapolate: true))?.x,
          near(q['extrapolated_x'], tol),
          reason: 'extrapolated $at',
        );
        expect(
          view.render(target, const RenderOpts(interpolate: false))?.x,
          near(q['raw_x'], tol),
          reason: 'raw $at',
        );
      }
      expect(view.overExtrapolations, i(section['over_extrapolations']),
          reason: 'the view accumulates what each render found');
    });
  });

  /// Integers throughout, so these are exact. That equality *is* the determinism
  /// guarantee: two peers that re-simulate to different states have no netcode.
  group('rollback', () {
    final v = load('vectors_rollback');

    test('two peers replay the Rust scenario frame for frame', () {
      final p0 = list(v['p0']).map(i).toList();
      final p1 = list(v['p1']).map(i).toList();
      final delay = i(v['delay_frames']);
      final config = RollbackConfig(maxRollbackFrames: i(v['max_rollback_frames']));

      List<int> step(List<int> state, List<int> inputs) =>
          [state[0] + inputs[0], state[1] + inputs[1]];

      RollbackSession<List<int>, int> session() => RollbackSession<List<int>, int>(
            initialState: const [0, 0],
            neutralInputs: const [0, 0],
            config: config,
            advance: step,
          );

      final a = session();
      final b = session();

      for (final frame in list(v['frames'])) {
        final fr = map(frame);
        final f = i(fr['frame']);
        a.queueLocalInput(0, p0[f]);
        b.queueLocalInput(1, p1[f]);
        if (f >= delay) {
          final past = f - delay;
          a.confirmRemoteInput(1, past, p1[past]);
          b.confirmRemoteInput(0, past, p0[past]);
        }
        a.advanceFrame();
        b.advanceFrame();

        expect(a.state, list(fr['a_pos']).map(i).toList(), reason: 'A at frame $f');
        expect(b.state, list(fr['b_pos']).map(i).toList(), reason: 'B at frame $f');
        expect(a.lastRollbackFrames, i(fr['a_last_rollback']), reason: 'A rollback at frame $f');
        expect(b.lastRollbackFrames, i(fr['b_last_rollback']), reason: 'B rollback at frame $f');
        expect(a.predictionHorizon, i(fr['a_horizon']), reason: 'A horizon at frame $f');
        expect(b.predictionHorizon, i(fr['b_horizon']), reason: 'B horizon at frame $f');
      }

      for (var f = p0.length - delay; f < p0.length; f++) {
        a.confirmRemoteInput(1, f, p1[f]);
        b.confirmRemoteInput(0, f, p0[f]);
      }
      a.advanceFrame();
      b.advanceFrame();

      final truth = list(v['ground_truth_pos_at_30']).map(i).toList();
      expect(a.stateAt(p0.length), list(v['a_settled_pos_at_30']).map(i).toList());
      expect(b.stateAt(p0.length), list(v['b_settled_pos_at_30']).map(i).toList());
      expect(a.stateAt(p0.length), truth, reason: 'peer A converged on the ground truth');
      expect(b.stateAt(p0.length), truth, reason: 'peer B converged on the ground truth');
      expect(a.rollbackCount, i(v['a_rollback_count']));
      expect(b.rollbackCount, i(v['b_rollback_count']));
      expect(a.maxRollbackFrames, i(v['a_max_rollback']));
      expect(b.maxRollbackFrames, i(v['b_max_rollback']));
    });

    /// A reset silently shortens how far back a session can roll: a correction
    /// arriving next frame finds nothing to restore. Contiguous saves must never
    /// cause one, which is the half of this worth asserting.
    test('StateHistory resets only on a non-contiguous save', () {
      final section = map(v['state_history']);
      final history = StateHistory<int>(i(section['capacity']));
      for (var frame = 0; frame < i(section['contiguous_saves']); frame++) {
        history.save(frame, frame * 10);
      }
      expect(history.resets, i(section['resets_while_contiguous']));

      history.save(500, -1);
      expect(history.resets, i(section['resets_after_a_jump']));
      expect(history.oldestFrame, i(section['oldest_frame_after_a_jump']));
      expect(history.latestFrame, i(section['latest_frame_after_a_jump']));
    });
  });

  /// The simulator's PRNG is what makes a scripted impairment scenario comparable
  /// across the two languages at all. Dart's `>>` sign-extends where Rust shifts a
  /// `u64` logically, and a divergence here would mean the two sides were quietly
  /// testing different networks.
  group('net_sim', () {
    final v = load('vectors_net_sim');

    test('the PRNG produces the same sequence', () {
      final rng = Rng(42);
      for (final draw in list(v['rng_seed_42'])) {
        final want = map(draw);
        expect(rng.upTo(1000), i(want['up_to_1000']));
        expect(rng.unit(), d(want['unit']));
      }
    });

    /// The case that needed the unsigned fold: a modulus this large keeps the top
    /// bit of the draw, which Dart reads as a negative number.
    test('the PRNG folds an unsigned draw across a wide modulus', () {
      final rng = Rng(0xDEADBEEF);
      final expected = list(v['rng_seed_deadbeef_up_to_u32_max']).map(i).toList();
      for (final want in expected) {
        expect(rng.upTo(0xFFFFFFFF), want);
      }
    });

    test('an ordered link delivers in the same order', () {
      final section = map(v['ordered_link']);
      final link = LatencyLink<int>();
      final rng = Rng(i(section['seed']));
      for (var seq = 0; seq < 40; seq++) {
        link.send(seq * 16, seq,
            latencyMs: i(section['latency_ms']), jitterMs: i(section['jitter_ms']), rng: rng);
      }
      expect(link.drainDue(100000), list(section['delivery']).map(i).toList());
    });

    test('an unordered link shuffles the same way', () {
      final section = map(v['unordered_link']);
      final link = LatencyLink<int>(ordering: PacketOrdering.unordered);
      final rng = Rng(i(section['seed']));
      for (var seq = 0; seq < 40; seq++) {
        link.send(seq * 16, seq,
            latencyMs: i(section['latency_ms']), jitterMs: i(section['jitter_ms']), rng: rng);
      }
      expect(link.drainDue(100000), list(section['delivery']).map(i).toList());
    });

    test('a lossy link drops the same packets', () {
      final section = map(v['lossy_link']);
      final link = LatencyLink<int>();
      final rng = Rng(i(section['seed']));
      for (var seq = 0; seq < i(section['sent']); seq++) {
        link.send(seq * 10, seq,
            latencyMs: i(section['latency_ms']), lossPct: d(section['loss_pct']), rng: rng);
      }
      expect(link.drainDue(100000), list(section['survived']).map(i).toList());
    });
  });
}
