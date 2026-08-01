import 'package:plaza_wire/plaza_wire.dart';
import 'package:test/test.dart';

void main() {
  group('externally tagged enums', () {
    /// The shape a property-check client silently drops.
    test('a unit variant is a bare string', () {
      expect(variantName('QueueLeft'), 'QueueLeft');
      expect(variantFields('QueueLeft'), isEmpty);
    });

    test('a struct variant is a one-entry map', () {
      const op = {
        'Placed': {'room_id': 'abc', 'spectator': false}
      };
      expect(variantName(op), 'Placed');
      expect(variantFields(op)['room_id'], 'abc');
      expect(variantFields(op)['spectator'], false);
    });

    test('a newtype variant gives its single value', () {
      const op = {
        'Snapshot': {'tick': 12}
      };
      expect(variantName(op), 'Snapshot');
      expect((variantBody(op) as Map)['tick'], 12);
    });

    test('a tuple variant gives a list', () {
      const op = {
        'Pair': [1, 2]
      };
      expect(variantBody(op), [1, 2]);
    });

    test('anything else is not a variant', () {
      expect(variantName(42), isNull);
      expect(variantName({'a': 1, 'b': 2}), isNull, reason: 'two keys is not a tag');
      expect(variantName(null), isNull);
    });

    test('building matches the shape serde expects', () {
      expect(variant('ListRooms'), 'ListRooms');
      expect(variant('Join', {'room_id': 'abc'}), {
        'Join': {'room_id': 'abc'}
      });
    });

    test('build and read round-trip', () {
      final unit = variant('Reroll');
      expect(variantName(unit), 'Reroll');
      final struct = variant('Grab', {'req': 7, 'item': 3});
      expect(variantFields(struct)['req'], 7);
    });
  });
}
