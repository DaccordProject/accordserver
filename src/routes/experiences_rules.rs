use super::experiences::Action;
use crate::{db::experiences::Session, error::AppError};
use serde_json::{json, Value};
use shakmaty::{fen::Fen, uci::UciMove, CastlingMode, Chess, Color, EnPassantMode, Position};

fn chess_state(position: &Chess, history: Vec<String>) -> Value {
    let mut board = vec![0; 64];
    for (square, piece) in position.board().iter() {
        board[usize::from(square)] =
            i32::from(piece.role) + if piece.color == Color::White { 0 } else { 6 };
    }
    json!({"fen":Fen::from_position(position, EnPassantMode::Legal).to_string(),"board":board,"history":history})
}

pub fn initial_chess() -> Value {
    chess_state(&Chess::default(), Vec::new())
}

pub fn chess_move(session: &mut Session, request: &Action, user: &str) -> Result<(), AppError> {
    if request.kind != "move" || !(0..64).contains(&request.a) || !(0..64).contains(&request.b) {
        return Err(AppError::BadRequest("Invalid chess action".into()));
    }
    let fen: Fen = session.game["fen"]
        .as_str()
        .unwrap_or("")
        .parse()
        .map_err(|_| AppError::Internal("Invalid game state".into()))?;
    let position: Chess = fen
        .into_position(CastlingMode::Standard)
        .map_err(|_| AppError::Internal("Invalid chess position".into()))?;
    let square = |s: i32| format!("{}{}", (b'a' + (s % 8) as u8) as char, s / 8 + 1);
    let promotion = request.promotion.as_deref().unwrap_or("");
    if !["", "q", "r", "b", "n"].contains(&promotion) {
        return Err(AppError::BadRequest("Invalid promotion".into()));
    }
    let uci: UciMove = format!("{}{}{promotion}", square(request.a), square(request.b))
        .parse()
        .map_err(|_| AppError::BadRequest("Invalid move".into()))?;
    let movement = uci
        .to_move(&position)
        .map_err(|_| AppError::BadRequest("Illegal move".into()))?;
    let next = position
        .play(movement)
        .map_err(|_| AppError::BadRequest("Illegal move".into()))?;
    let mut history: Vec<String> =
        serde_json::from_value(session.game["history"].clone()).unwrap_or_default();
    if history.len() >= 2048 {
        session.end("move_limit_draw", None);
        return Ok(());
    }
    history.push(
        session.game["fen"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .take(4)
            .collect::<Vec<_>>()
            .join(" "),
    );
    let next_fen = Fen::from_position(&next, EnPassantMode::Legal).to_string();
    let repetition = next_fen
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ");
    if next.is_checkmate() {
        session.end("checkmate", Some(user.to_string()));
    } else if next.is_stalemate()
        || next.is_insufficient_material()
        || next.halfmoves() >= 100
        || history.iter().filter(|f| *f == &repetition).count() >= 2
    {
        session.end("draw", None);
    } else {
        let slot = if next.turn() == Color::White { 0 } else { 1 };
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
