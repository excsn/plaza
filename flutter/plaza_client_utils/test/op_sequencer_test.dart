import 'dart:async';

import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Applies everything, holding for whatever `holds` says.
({List<Object?> applied, Hold Function(Object?) apply}) recorder(Map<Object?, Hold> holds) {
  final applied = <Object?>[];
  return (
    applied: applied,
    apply: (Object? op) {
      applied.add(op);
      return holds[op] ?? Hold.none;
    },
  );
}

void main() {
  test('ops with nothing to watch drain in one pump', () {
    final q = OpSequencer<Object?>();
    final r = recorder({});
    q.addAll(['a', 'b', 'c']);

    q.pump(0.016, r.apply);

    expect(r.applied, ['a', 'b', 'c']);
    expect(q.pending, 0);
  });

  test('a hold stops the queue at the op worth seeing', () {
    final q = OpSequencer<Object?>();
    final r = recorder({'b': const Hold.seconds(0.5)});
    q.addAll(['a', 'b', 'c']);

    q.pump(0.016, r.apply);

    expect(r.applied, ['a', 'b'], reason: 'c must wait behind b');
    expect(q.pending, 1);
    expect(q.holding, isTrue);
  });

  test('the queue resumes once the hold elapses', () {
    final q = OpSequencer<Object?>();
    final r = recorder({'b': const Hold.seconds(0.5)});
    q.addAll(['a', 'b', 'c']);

    q.pump(0.016, r.apply);
    q.pump(0.3, r.apply);
    expect(r.applied, ['a', 'b'], reason: 'half a hold is not a hold');

    q.pump(0.3, r.apply);
    expect(r.applied, ['a', 'b', 'c']);
    expect(q.holding, isFalse);
  });

  test('leftover time carries into the next hold rather than being discarded', () {
    final q = OpSequencer<Object?>();
    final r = recorder({'a': const Hold.seconds(0.1), 'b': const Hold.seconds(0.1)});
    q.addAll(['a', 'b', 'c']);

    q.pump(0.016, r.apply);
    expect(r.applied, ['a']);

    q.pump(0.25, r.apply);
    expect(r.applied, ['a', 'b', 'c'], reason: 'one long frame should clear both holds');
  });

  test('an op arriving mid-hold waits its turn', () {
    final q = OpSequencer<Object?>();
    final r = recorder({'a': const Hold.seconds(0.5)});
    q.add('a');
    q.pump(0.016, r.apply);
    expect(r.applied, ['a']);

    q.add('b');
    q.pump(0.1, r.apply);
    expect(r.applied, ['a'], reason: 'b jumped the hold');

    q.pump(0.5, r.apply);
    expect(r.applied, ['a', 'b']);
  });

  test('a future hold is released on the first pump after it completes', () async {
    final q = OpSequencer<Object?>();
    final done = Completer<void>();
    final r = recorder({'a': Hold.until(done.future)});
    q.addAll(['a', 'b']);

    q.pump(0.016, r.apply);
    q.pump(5, r.apply);
    expect(r.applied, ['a'], reason: 'time does not release a future hold');
    expect(q.holding, isTrue);

    done.complete();
    await Future<void>.value();
    expect(r.applied, ['a'], reason: 'nothing is released outside a pump');

    q.pump(0.016, r.apply);
    expect(r.applied, ['a', 'b']);
    expect(q.holding, isFalse);
  });

  test('a future that fails releases the hold too', () async {
    final q = OpSequencer<Object?>();
    final failed = Completer<void>();
    final r = recorder({'a': Hold.until(failed.future)});
    q.addAll(['a', 'b']);

    q.pump(0.016, r.apply);
    failed.completeError(StateError('animation cancelled'));
    await Future<void>.value();
    q.pump(0.016, r.apply);

    expect(r.applied, ['a', 'b']);
  });

  test('clear drops the backlog, which is what a resume needs', () {
    final q = OpSequencer<Object?>();
    final r = recorder({});
    q.addAll(['a', 'b', 'c']);

    q.clear();
    q.pump(0.016, r.apply);

    expect(r.applied, isEmpty);
    expect(q.pending, 0);
  });

  test('clear drops the hold in progress, so fresh state is not made to wait', () {
    final q = OpSequencer<Object?>();
    final r = recorder({'a': const Hold.seconds(5)});
    q.add('a');
    q.pump(0.016, r.apply);
    expect(q.holding, isTrue);

    q.clear();
    q.add('b');
    q.pump(0.016, r.apply);

    expect(r.applied, ['a', 'b']);
  });

  test('a future completing after a clear releases nothing', () async {
    final q = OpSequencer<Object?>();
    final stale = Completer<void>();
    final fresh = Completer<void>();
    final r = recorder({'a': Hold.until(stale.future), 'b': Hold.until(fresh.future)});
    q.add('a');
    q.pump(0.016, r.apply);

    q.clear();
    q.addAll(['b', 'c']);
    q.pump(0.016, r.apply);
    expect(r.applied, ['a', 'b']);

    stale.complete();
    await Future<void>.value();
    q.pump(0.016, r.apply);
    expect(r.applied, ['a', 'b'], reason: 'the stale future released the new hold');

    fresh.complete();
    await Future<void>.value();
    q.pump(0.016, r.apply);
    expect(r.applied, ['a', 'b', 'c']);
  });

  test('overflow is counted rather than silent', () {
    final q = OpSequencer<Object?>(maxQueued: 2);
    q.addAll(['a', 'b', 'c', 'd']);

    expect(q.pending, 2);
    expect(q.dropped, 2);
  });

  test('pumping an empty queue is not an error', () {
    final q = OpSequencer<Object?>();
    final r = recorder({});
    q.pump(0.016, r.apply);
    expect(r.applied, isEmpty);
  });
}
