//! Golden encodings of parlour's ops, for the Dart generated-types conformance
//! suite. Every variant kind is represented: unit, newtype, struct, boxed
//! snapshot, and the vocabulary payloads with their options both present and
//! absent.
//!
//! Regenerate after a wire change:
//!     PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_example_parlour_game --test wire_fixtures

use plaza_example_parlour_game::types::{
  Card, LinkQuality, LobbyOp, PlayerView, RoundSummary, Seat, TableCard, TableOp, TablePhase,
};
use plaza_wire::flow_payloads::{
  PhaseChangedNoticePayload, RoundEndedNoticePayload, RoundStartedNoticePayload, TurnChangedNoticePayload,
};
use plaza_wire::{JsonCodec, MsgPackCodec, MsgPackNamedCodec, WireCodec};
use std::time::Duration;

fn table_ops() -> Vec<TableOp> {
  vec![
    TableOp::PlayCard(Card(7)),
    TableOp::Snapshot(Box::new(PlayerView {
      table: "golden".into(),
      phase: TablePhase::Playing,
      round: 2,
      total_rounds: Some(3),
      whose_turn: Some(11),
      your_seat: Some(Seat::Player),
      stake: 25,
      coins: 100,
      my_hand: vec![Card(4), Card(9)],
      opponents: vec![(12, 3), (13, 2)],
      played: vec![(11, Card(6))],
      scores: vec![(11, 1), (12, 0)],
      seats_taken: 3,
      seats_total: 3,
      spectators: 1,
      bots: 1,
    })),
    TableOp::Snapshot(Box::new(PlayerView {
      table: "spectating".into(),
      phase: TablePhase::Seating,
      round: 0,
      total_rounds: None,
      whose_turn: None,
      your_seat: None,
      stake: 0,
      coins: 0,
      my_hand: Vec::new(),
      opponents: Vec::new(),
      played: Vec::new(),
      scores: Vec::new(),
      seats_taken: 0,
      seats_total: 3,
      spectators: 0,
      bots: 0,
    })),
    TableOp::CardPlayed { player: 11, card: Card(6) },
    TableOp::PlayedForYou { player: 12, card: Card(2) },
    TableOp::TrickWon { player: 11, card: Card(6) },
    TableOp::Settled { winner: Some(11), coins: 150 },
    TableOp::Settled { winner: None, coins: 100 },
    TableOp::Rejected { reason: "not your turn".into() },
    TableOp::Closed { reason: "table folded".into() },
    TableOp::PhaseChanged(PhaseChangedNoticePayload {
      new_phase: TablePhase::Scoring,
      previous_phase: Some(TablePhase::Playing),
      duration_hint: Some(Duration::from_millis(1500)),
      reason: Some("trick resolved".into()),
    }),
    TableOp::TurnChanged(TurnChangedNoticePayload {
      new_turn_actor: Some(12),
      previous_turn_actor: Some(11),
      turn_number: 4,
      time_limit_for_turn: None,
    }),
    TableOp::RoundStarted(RoundStartedNoticePayload {
      round_number: 2,
      total_rounds: Some(3),
    }),
    TableOp::RoundEnded(RoundEndedNoticePayload {
      round_number: 2,
      reason: "all cards down".into(),
      summary_data: Some(RoundSummary {
        winner: Some(11),
        winning_card: Some(Card(6)),
      }),
    }),
  ]
}

fn lobby_ops() -> Vec<LobbyOp> {
  let link = LinkQuality::new(48, 20);
  vec![
    LobbyOp::ListTables,
    LobbyOp::QuickMatch,
    LobbyOp::Welcome { you: 11, link, coins: 100 },
    LobbyOp::Catalogue {
      tables: vec![TableCard {
        room_id: uuid::Uuid::from_u128(0x00112233_4455_6677_8899_aabbccddeeff),
        name: "penny table".into(),
        current_players: 2,
        max_players: 3,
        budget_ms: Some(90),
        playable: true,
        fit_rank: Some(0),
      }],
      link,
    },
    LobbyOp::Queued { position: 0, needed: 2, patience_ms: 4000 },
    LobbyOp::QueueLeft,
    LobbyOp::Placed {
      room_id: uuid::Uuid::from_u128(1),
      name: "penny table".into(),
      endpoint: "ws://127.0.0.1:8092/table/1?ticket=t".into(),
      spectator: false,
      coins: 100,
    },
    LobbyOp::Refused {
      room_id: uuid::Uuid::from_u128(1),
      reason: "your link cannot carry it".into(),
      measured_one_way_ms: 210,
      allowed_one_way_ms: Some(90),
    },
  ]
}

fn golden(name: &str, bytes: &[u8]) {
  let path = format!("{}/../../flutter/fixtures/parlour/{name}", env!("CARGO_MANIFEST_DIR"));
  if std::env::var("PLAZA_REGENERATE_FIXTURES").is_ok() {
    std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    return;
  }
  let committed = std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}; regenerate with PLAZA_REGENERATE_FIXTURES=1"));
  assert_eq!(
    committed, bytes,
    "{name} drifted from the wire; regenerate with PLAZA_REGENERATE_FIXTURES=1 and rerun the Dart conformance suite"
  );
}

#[test]
fn the_golden_encodings_match_the_wire() {
  golden("table_ops.msgpack", &MsgPackCodec.encode(&table_ops()).unwrap());
  golden("table_ops.named.msgpack", &MsgPackNamedCodec.encode(&table_ops()).unwrap());
  golden("lobby_ops.json", &JsonCodec.encode(&lobby_ops()).unwrap());
}
