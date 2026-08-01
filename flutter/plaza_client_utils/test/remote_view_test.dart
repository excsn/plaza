import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

class S {
  const S(this.x);
  final double x;

  @override
  String toString() => 'S($x)';
}

S lerp(S a, S b, double t) => S(a.x + (b.x - a.x) * t);
S extrapolate(S s, double velocity, double dtSecs) => S(s.x + velocity * dtSecs);

RemoteView<S, double> view() => RemoteView<S, double>(
      bufferSize: 8,
      maxExtrapolationMs: 500,
      lerp: lerp,
      extrapolateBy: extrapolate,
    );

const interpolating = RenderOpts();
const dead = RenderOpts(interpolate: false);
const reckoning = RenderOpts(extrapolate: true);

/// Transliterated from `client_utils/src/remote_view.rs`.
void main() {
  test('nothing renders before the first push', () {
    expect(view().render(100), isNull);
  });

  test('it interpolates between snapshots', () {
    final v = view()
      ..push(100, const S(10.0), 0.0)
      ..push(200, const S(20.0), 0.0);
    expect(v.render(150, interpolating)!.x, closeTo(15.0, 1e-3));
  });

  test('interpolation off renders the raw newest', () {
    final v = view()
      ..push(100, const S(10.0), 0.0)
      ..push(200, const S(20.0), 0.0);
    expect(v.render(150, dead)!.x, 20.0, reason: 'newest snapshot, not interpolated');
  });

  test('it dead reckons past the newest when asked', () {
    final v = view()
      ..push(100, const S(0.0), 10.0)
      ..push(200, const S(1.0), 10.0);

    // 500ms past the newest, at 10/s, projects +5.
    expect(v.render(700, reckoning)!.x, closeTo(6.0, 0.2));

    // Without extrapolation, it holds the newest.
    expect(v.render(700, interpolating)!.x, closeTo(1.0, 1e-3), reason: 'held at the newest');
  });

  test('a late out-of-order snapshot does not become the newest', () {
    final v = view()
      ..push(200, const S(20.0), 1.0)
      ..push(100, const S(10.0), 9.0);

    expect(v.latest!.x, 20.0, reason: 'the extrapolation base stays at t=200');
    expect(
      v.render(150, interpolating)!.x,
      closeTo(15.0, 1e-3),
      reason: 'the straggler still made it into the buffer',
    );
  });

  test('duplicate timestamps do not throw', () {
    final v = view()
      ..push(100, const S(10.0), 0.0)
      ..push(100, const S(11.0), 0.0)
      ..push(200, const S(20.0), 0.0);
    expect(v.render(150)!.x.isFinite, isTrue);
  });

  test('a single snapshot renders that snapshot', () {
    final v = view()..push(100, const S(7.0), 0.0);
    expect(v.render(150)!.x, 7.0, reason: 'one snapshot cannot bracket a target');
  });

  /// The cap must not fly off along the velocity, and must not rewind to the newest
  /// sample either: it holds where the cap stopped it, which is the only continuous
  /// answer.
  test('extrapolation holds at the cap rather than flying off or snapping back', () {
    final v = view()
      ..push(100, const S(0.0), 10.0)
      ..push(200, const S(1.0), 10.0);

    final atCap = v.render(700, reckoning)!;
    final far = v.render(2200, reckoning)!;
    expect(far.x, closeTo(atCap.x, 1e-3), reason: 'past the cap it must hold steady');
    expect(far.x, greaterThan(1.0), reason: 'and hold at the cap, not back at the newest sample');

    // The boundary itself is continuous, which is the property a jittery target
    // crossing it back and forth depends on.
    final inside = v.render(699, reckoning)!;
    final outside = v.render(701, reckoning)!;
    expect(outside.x, closeTo(inside.x, 0.05), reason: 'crossing the cap must not jump');
  });

  /// Not in the Rust original, which logs a warning. Reaching the cap steadily is
  /// almost never a starved link, it is a render target computed the wrong way.
  test('renders past the cap are counted', () {
    final v = view()..push(100, const S(0.0), 10.0);
    expect(v.overExtrapolations, 0);
    v.render(400, reckoning);
    expect(v.overExtrapolations, 0, reason: 'inside the cap');
    v.render(5000, reckoning);
    expect(v.overExtrapolations, 1);
  });

  test('a null target draws the newest', () {
    final v = view()
      ..push(100, const S(10.0), 0.0)
      ..push(200, const S(20.0), 0.0);
    expect(v.render(null)!.x, 20.0);
  });
}
