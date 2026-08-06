import 'package:flutter_test/flutter_test.dart';
import 'package:parlour_client/sequencer.dart';

void main() {
  /// Applies everything, holding for whatever `holds` says.
  ({List<Object?> applied, double Function(Object?) apply}) recorder(Map<Object?, double> holds) {
    final applied = <Object?>[];
    return (
      applied: applied,
      apply: (Object? op) {
        applied.add(op);
        return holds[op] ?? 0;
      },
    );
  }

  test('ops with nothing to watch drain in one pump', () {
    final q = OpSequencer();
    final r = recorder({});
    q.addAll(['a', 'b', 'c']);

    q.pump(0.016, r.apply);

    expect(r.applied, ['a', 'b', 'c']);
    expect(q.pending, 0);
  });

  test('a hold stops the queue at the op worth seeing', () {
    final q = OpSequencer();
    final r = recorder({'b': 0.5});
    q.addAll(['a', 'b', 'c']);

    q.pump(0.016, r.apply);

    expect(r.applied, ['a', 'b'], reason: 'c must wait behind b');
    expect(q.pending, 1);
    expect(q.holding, isTrue);
  });

  test('the queue resumes once the hold elapses', () {
    final q = OpSequencer();
    final r = recorder({'b': 0.5});
    q.addAll(['a', 'b', 'c']);

    q.pump(0.016, r.apply);
    q.pump(0.3, r.apply);
    expect(r.applied, ['a', 'b'], reason: 'half a hold is not a hold');

    q.pump(0.3, r.apply);
    expect(r.applied, ['a', 'b', 'c']);
    expect(q.holding, isFalse);
  });

  /// A slow frame must not silently stretch every hold behind it. Two holds of
  /// 0.1 inside one 0.25 frame both elapse, and the leftover carries forward.
  test('leftover time carries into the next hold rather than being discarded', () {
    final q = OpSequencer();
    final r = recorder({'a': 0.1, 'b': 0.1});
    q.addAll(['a', 'b', 'c']);

    q.pump(0.016, r.apply);
    expect(r.applied, ['a']);

    q.pump(0.25, r.apply);
    expect(r.applied, ['a', 'b', 'c'], reason: 'one long frame should clear both holds');
  });

  test('an op arriving mid-hold waits its turn', () {
    final q = OpSequencer();
    final r = recorder({'a': 0.5});
    q.add('a');
    q.pump(0.016, r.apply);
    expect(r.applied, ['a']);

    q.add('b');
    q.pump(0.1, r.apply);
    expect(r.applied, ['a'], reason: 'b jumped the hold');

    q.pump(0.5, r.apply);
    expect(r.applied, ['a', 'b']);
  });

  test('clear drops the backlog, which is what a resume needs', () {
    final q = OpSequencer();
    final r = recorder({});
    q.addAll(['a', 'b', 'c']);

    q.clear();
    q.pump(0.016, r.apply);

    expect(r.applied, isEmpty);
    expect(q.pending, 0);
  });

  /// A sequencer that quietly drops is indistinguishable from a server that
  /// never sent, so the overflow is counted rather than silent.
  test('overflow is counted rather than silent', () {
    final q = OpSequencer(maxQueued: 2);
    q.addAll(['a', 'b', 'c', 'd']);

    expect(q.pending, 2);
    expect(q.dropped, 2);
  });

  test('pumping an empty queue is not an error', () {
    final q = OpSequencer();
    final r = recorder({});
    q.pump(0.016, r.apply);
    expect(r.applied, isEmpty);
  });
}
