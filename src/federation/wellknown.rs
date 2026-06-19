//! Serves this server's federation metadata at
//! `GET /.well-known/accord-federation` so peers can discover our public key
//! and inbox URL.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;

use crate::federation::peers::WellKnown;
use crate::state::AppState;

pub async fn handle_well_known(State(state): State<AppState>) -> axum::response::Response {
    let Some(fed) = state.federation.as_ref() else {
        return (StatusCode::NOT_FOUND, "federation disabled").into_response();
    };

    let doc = WellKnown {
        domain: fed.domain.clone(),
        public_key: fed.identity.public_key_b64(),
        inbox_url: fed.inbox_url(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    Json(doc).into_response()
}
