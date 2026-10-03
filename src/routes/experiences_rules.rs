use super::experiences::Action;
use crate::{db::experiences::Session, error::AppError};
use cozy_chess::{util::parse_uci_move, Board, Color, GameStatus, Piece};
use serde_json::{json, Value};

fn chess_state(position: &Board, history: Vec<String>) -> Value {
    let mut board = vec![0; 64];
    for color in [Color::White, Color::Black] {
        for piece in Piece::ALL {
            for square in position.colors(color) & position.pieces(piece) {
                board[square as usize] =
                    piece as i32 + 1 + if color == Color::White { 0 } else { 6 };
            }
        }
    }
    json!({"fen":position.to_string(),"board":board,"history":history})
}

fn insufficient_material(position: &Board) -> bool {
    if !(position.pieces(Piece::Pawn)
        | position.pieces(Piece::Rook)
        | position.pieces(Piece::Queen))
    .is_empty()
    {
        return false;
    }
    let bishops = position.pieces(Piece::Bishop);
    let knights = position.pieces(Piece::Knight);
    if (bishops | knights).len() <= 1 {
        return true;
    }
    if !knights.is_empty() {
        return false;
    }
    // With only bishops on the same color complex, neither side can mate.
    let colors: Vec<_> = bishops
        .into_iter()
        .map(|s| (s.file() as usize + s.rank() as usize) % 2)
        .collect();
    colors.iter().all(|c| Some(c) == colors.first())
}

pub fn initial_chess() -> Value {
    chess_state(&Board::default(), Vec::new())
}

pub fn chess_move(session: &mut Session, request: &Action, user: &str) -> Result<(), AppError> {
    if request.kind != "move" || !(0..64).contains(&request.a) || !(0..64).contains(&request.b) {
        return Err(AppError::BadRequest("Invalid chess action".into()));
    }
    let mut next = Board::from_fen(session.game["fen"].as_str().unwrap_or(""), false)
        .map_err(|_| AppError::Internal("Invalid chess position".into()))?;
    let square = |s: i32| format!("{}{}", (b'a' + (s % 8) as u8) as char, s / 8 + 1);
    let promotion = request.promotion.as_deref().unwrap_or("");
    if !["", "q", "r", "b", "n"].contains(&promotion) {
        return Err(AppError::BadRequest("Invalid promotion".into()));
    }
    let movement = parse_uci_move(
        &next,
        &format!("{}{}{promotion}", square(request.a), square(request.b)),
    )
    .map_err(|_| AppError::BadRequest("Invalid move".into()))?;
    next.try_play(movement)
        .map_err(|_| AppError::BadRequest("Illegal move".into()))?;
    let mut history: Vec<String> =
        serde_json::from_value(session.game["history"].clone()).unwrap_or_default();
    if history.len() >= 2048 {
        session.end("move_limit_draw", None);
        return Ok(());
    }
    history.push(session.game["fen"].as_str().unwrap().to_string());
    // Read legacy four-field repetition entries as well as complete FENs.
    // same_position ignores clocks and ineffective/pinned en-passant squares.
    let repetitions = history
        .iter()
        .filter(|fen| {
            let fen = if fen.split_whitespace().count() == 4 {
                format!("{fen} 0 1")
            } else {
                (*fen).clone()
            };
            Board::from_fen(&fen, false).is_ok_and(|past| next.same_position(&past))
        })
        .count();
    if next.status() == GameStatus::Won {
        session.end("checkmate", Some(user.to_string()));
    } else if next.status() == GameStatus::Drawn || insufficient_material(&next) || repetitions >= 2
    {
        session.end("draw", None);
    } else {
        let slot = if next.side_to_move() == Color::White {
            0
        } else {
            1
        };
        session.turn_user_id = session
            .participants
            .iter()
            .find(|p| p.slot == Some(slot))
            .map(|p| p.user_id.clone());
        if session.turn_timeout_seconds > 0 {
            session.deadline = Some(chrono::Utc::now().timestamp() + session.turn_timeout_seconds);
        }
    }
    session.game = chess_state(&next, history);
    Ok(())
}

pub fn initial_pong() -> Value {
    json!({"rects":[32,400,24,160,968,400,24,160,500,500,24,24],"velocity":[320,180],"score":[0,0],"tick":0,"last_tick_ms":chrono::Utc::now().timestamp_millis(),"paused":false})
}

/// Server-authoritative fixed-step Pong. Input never supplies ball/score state.
pub fn pong_input(session: &mut Session, slot: usize, target: i32) -> Result<(), AppError> {
    if slot > 1 || !(0..=864).contains(&target) {
        return Err(AppError::BadRequest("Invalid paddle input".into()));
    }
    session.game["rects"][slot * 4 + 1] = json!(target);
    Ok(())
}

pub fn pong_tick(session: &mut Session, now: i64) {
    let last = session.game["last_tick_ms"].as_i64().unwrap_or(now);
    session.game["last_tick_ms"] = json!(now);
    // Never fast-forward missed ticks after a pause/reconnect.
    let steps = ((now - last).clamp(0, 100) / 16).min(6);
    if session.game["paused"] == true {
        return;
    }
    let mut rects: Vec<i64> = serde_json::from_value(session.game["rects"].clone()).unwrap();
    let mut velocity: Vec<i64> = serde_json::from_value(session.game["velocity"].clone()).unwrap();
    let mut score: Vec<i64> = serde_json::from_value(session.game["score"].clone()).unwrap();
    for _ in 0..steps {
        rects[8] += velocity[0] * 16 / 1000;
        rects[9] += velocity[1] * 16 / 1000;
        if rects[9] <= 0 || rects[9] >= 1000 {
            velocity[1] = -velocity[1];
            rects[9] = rects[9].clamp(0, 1000);
        }
        for paddle in 0..2 {
            let offset = paddle * 4;
            if rects[8] < rects[offset] + rects[offset + 2]
                && rects[8] + 24 > rects[offset]
                && rects[9] < rects[offset + 1] + 160
                && rects[9] + 24 > rects[offset + 1]
                && ((paddle == 0 && velocity[0] < 0) || (paddle == 1 && velocity[0] > 0))
            {
                velocity[0] = -velocity[0];
            }
        }
        if rects[8] < 0 || rects[8] > 1024 {
            let winner = if rects[8] < 0 { 1 } else { 0 };
            score[winner] += 1;
            rects[8] = 500;
            rects[9] = 500;
            if score[winner] >= 5 {
                let user = session
                    .participants
                    .iter()
                    .find(|p| p.slot == Some(winner))
                    .map(|p| p.user_id.clone());
                session.end("score", user);
                break;
            }
        }
    }
    session.game["rects"] = json!(rects);
    session.game["velocity"] = json!(velocity);
    session.game["score"] = json!(score);
    session.game["tick"] = json!(session.game["tick"].as_i64().unwrap_or(0) + steps);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::experiences::Participant;
    fn session(fen: &str) -> Session {
        let position = Board::from_fen(fen, false).unwrap();
        Session {
            id: "game".into(),
            space_id: "space".into(),
            game_id: "chess".into(),
            version: "1.0.0".into(),
            digest: "digest".into(),
            installation_generation: 1,
            mode: "turn_based".into(),
            state: "running".into(),
            host_user_id: "white".into(),
            invite_only: false,
            invited: vec![],
            participants: vec![
                Participant {
                    user_id: "white".into(),
                    role: "player".into(),
                    slot: Some(0),
                    ready: true,
                    last_seen: 0,
                },
                Participant {
                    user_id: "black".into(),
                    role: "player".into(),
                    slot: Some(1),
                    ready: true,
                    last_seen: 0,
                },
            ],
            revision: 0,
            game: chess_state(&position, vec![]),
            result: None,
            turn_user_id: Some("white".into()),
            turn_timeout_seconds: 0,
            deadline: None,
            created_at: 0,
            updated_at: 0,
            last_activity_at: 0,
            idle_expires_at: Some(crate::db::experiences::IDLE_TIMEOUT_SECONDS),
        }
    }
    fn play(
        session: &mut Session,
        a: i32,
        b: i32,
        promotion: Option<&str>,
    ) -> Result<(), AppError> {
        let user = session
            .turn_user_id
            .clone()
            .unwrap_or_else(|| "white".into());
        chess_move(
            session,
            &Action {
                revision: session.revision,
                kind: "move".into(),
                a,
                b,
                promotion: promotion.map(str::to_string),
            },
            &user,
        )
    }
    #[test]
    fn castling_moves_the_king_and_rook_using_standard_client_coordinates() {
        let mut game = session("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1");
        play(&mut game, 4, 6, None).unwrap();
        assert_eq!(game.game["board"][6], 6);
        assert_eq!(game.game["board"][5], 4);
        assert_eq!(game.game["board"][4], 0);
        assert_eq!(game.game["board"][7], 0);
    }
    #[test]
    fn pinned_pieces_cannot_expose_their_king() {
        let mut game = session("k3r3/8/8/8/8/8/4R3/4K3 w - - 0 1");
        let before = game.game.clone();
        assert!(play(&mut game, 12, 13, None).is_err());
        assert_eq!(game.game, before);
    }
    #[test]
    fn en_passant_and_promotion_update_the_authoritative_board() {
        let mut game = session("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 2");
        play(&mut game, 36, 43, None).unwrap();
        assert_eq!(game.game["board"][43], 1);
        assert_eq!(game.game["board"][35], 0);
        let mut game = session("4k3/P7/8/8/8/8/8/4K3 w - - 0 1");
        play(&mut game, 48, 56, Some("q")).unwrap();
        assert_eq!(game.game["board"][56], 5);
    }
    #[test]
    fn repetition_ignores_clocks_and_effective_en_passant() {
        let mut game = session(&Board::default().to_string());
        for _ in 0..2 {
            for (a, b) in [(6, 21), (62, 45), (21, 6), (45, 62)] {
                play(&mut game, a, b, None).unwrap();
            }
        }
        assert_eq!(game.state, "ended");
        assert_eq!(game.result.unwrap()["outcome"], "draw");
        let with_ep = Board::from_fen(
            "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1",
            false,
        )
        .unwrap();
        let without_ep = Board::from_fen(
            "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 4 3",
            false,
        )
        .unwrap();
        assert!(with_ep.same_position(&without_ep));
    }
    #[test]
    fn stalemate_fifty_moves_and_dead_material_record_draws() {
        for (fen, a, b) in [
            ("7k/5Q2/5K2/8/8/8/8/8 w - - 0 1", 53, 46),
            ("4k3/8/8/8/8/8/8/R3K3 w - - 99 1", 0, 8),
            ("4k3/8/8/8/8/8/8/2B1K3 w - - 0 1", 2, 11),
        ] {
            let mut game = session(fen);
            play(&mut game, a, b, None).unwrap();
            assert_eq!(game.state, "ended", "{fen}");
            assert_eq!(game.result.unwrap()["outcome"], "draw");
        }
    }
}
