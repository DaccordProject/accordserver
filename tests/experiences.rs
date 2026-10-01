mod common;
use axum::{
    body::to_bytes,
    http::{Method, StatusCode},
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use common::{authenticated_json_request, authenticated_request, TestServer, TestUser};
use ed25519_dalek::{Signer, SigningKey};
use experience_contract::{digest, validate_package, Release};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use serial_test::serial;
use std::sync::{Arc, RwLock};
use tower::ServiceExt;

async fn call(
    server: &TestServer,
    user: &TestUser,
    method: Method,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = if method == Method::GET || method == Method::DELETE {
        authenticated_request(method, path, &user.auth_header())
    } else {
        authenticated_json_request(method, path, &user.auth_header(), &body)
    };
    let response = server.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 3_000_000).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!({})))
}

async fn ok(
    server: &TestServer,
    user: &TestUser,
    method: Method,
    path: &str,
    body: Value,
) -> Value {
    let (status, value) = call(server, user, method, path, body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value["data"].clone()
}

async fn start_chess(server: &TestServer, owner: &TestUser, black: &TestUser, base: &str) -> Value {
    let mut session = ok(
        server,
        owner,
        Method::POST,
        &format!("{base}/arcade/sessions"),
        json!({"game_id":"chess"}),
    )
    .await;
    let path = format!(
        "{base}/arcade/sessions/{}/members",
        session["id"].as_str().unwrap()
    );
    session = ok(
        server,
        black,
        Method::POST,
        &path,
        json!({"operation":"join","revision":session["revision"]}),
    )
    .await;
    for user in [owner, black] {
        session = ok(
            server,
            user,
            Method::POST,
            &path,
            json!({"operation":"ready","ready":true,"revision":session["revision"]}),
        )
        .await;
    }
    ok(
        server,
        owner,
        Method::POST,
        &path,
        json!({"operation":"start","revision":session["revision"]}),
    )
    .await
}

fn release(game: &str) -> Release {
    let payload = if game == "chess" {
        include_bytes!("fixtures/chess.json").as_slice()
    } else {
        include_bytes!("fixtures/pong.json").as_slice()
    };
    let package = validate_package(payload).unwrap();
    Release {
        manifest: package.manifest,
        payload: STANDARD.encode(payload),
        digest: digest(payload),
        key_id: "test".into(),
        signature: SigningKey::from_bytes(&[1; 32])
            .sign(payload)
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        status: "approved".into(),
        reviewed_at: 1,
    }
}

#[tokio::test]
#[serial]
async fn directory_lobbies_chess_authority_resume_and_revocation() {
    let trust = SigningKey::from_bytes(&[1; 32])
        .verifying_key()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    std::env::set_var("EXPERIENCES_ENABLED", "true");
    std::env::set_var("EXPERIENCE_TRUSTED_KEYS", json!({"test":trust}).to_string());
    let release = Arc::new(RwLock::new(release("chess")));
    let pong_release = crate::release("pong");
    let item = release.clone();
    let catalogue = release.clone();
    let app = Router::new()
        .route(
            "/api/v1/experiences/chess/1.0.0",
            get(move || {
                let item = item.clone();
                async move { Json(json!({"data":item.read().unwrap().clone()})) }
            }),
        )
        .route(
            "/api/v1/experiences",
            get(move || {
                let catalogue = catalogue.clone();
                async move { Json(json!({"data":[catalogue.read().unwrap().clone()]})) }
            }),
        );
    let pong_copy = pong_release.clone();
    let app = app.route(
        "/api/v1/experiences/pong/1.0.0",
        get(move || {
            let release = pong_copy.clone();
            async move { Json(json!({"data":release})) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    std::env::set_var(
        "EXPERIENCE_DIRECTORY_URL",
        format!("http://{}", listener.local_addr().unwrap()),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("game-owner").await;
    let black = server.create_user_with_token("game-black").await;
    let spectator = server.create_user_with_token("game-spectator").await;
    let outsider = server.create_user_with_token("game-outsider").await;
    let space = server.create_space(&owner.user.id, "Arcade").await;
    for user in [&black, &spectator] {
        server.add_member(&space, &user.user.id).await;
    }
    let base = format!("/api/v1/spaces/{space}");
    let game = format!("{base}/experiences/chess");
    assert_eq!(
        call(
            &server,
            &black,
            Method::PUT,
            &game,
            json!({"version":"1.0.0"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    ok(
        &server,
        &owner,
        Method::PUT,
        &game,
        json!({"version":"1.0.0"}),
    )
    .await;
    assert_eq!(
        ok(
            &server,
            &owner,
            Method::GET,
            &format!("{base}/arcade"),
            Value::Null
        )
        .await["visible"],
        true
    );
    assert_eq!(
        call(
            &server,
            &outsider,
            Method::GET,
            &format!("{game}/package"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut session = ok(
        &server,
        &owner,
        Method::POST,
        &format!("{base}/arcade/sessions"),
        json!({"game_id":"chess"}),
    )
    .await;
    let path = format!("{base}/arcade/sessions/{}", session["id"].as_str().unwrap());
    assert_eq!(
        call(&server, &outsider, Method::GET, &path, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    session = ok(
        &server,
        &black,
        Method::POST,
        &format!("{path}/members"),
        json!({"operation":"join","revision":session["revision"]}),
    )
    .await;
    session = ok(
        &server,
        &spectator,
        Method::POST,
        &format!("{path}/members"),
        json!({"operation":"join","spectator":true,"revision":session["revision"]}),
    )
    .await;
    assert_eq!(
        call(
            &server,
            &spectator,
            Method::POST,
            &format!("{path}/members"),
            json!({"operation":"ready","ready":true,"revision":session["revision"]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for user in [&owner, &black] {
        session = ok(
            &server,
            user,
            Method::POST,
            &format!("{path}/members"),
            json!({"operation":"ready","ready":true,"revision":session["revision"]}),
        )
        .await;
    }
    session = ok(
        &server,
        &owner,
        Method::POST,
        &format!("{path}/members"),
        json!({"operation":"start","revision":session["revision"]}),
    )
    .await;
    assert_eq!(session["state"], "running");
    assert_eq!(
        call(
            &server,
            &black,
            Method::POST,
            &format!("{path}/actions"),
            json!({"kind":"move","a":52,"b":36,"revision":session["revision"]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &server,
            &spectator,
            Method::POST,
            &format!("{path}/actions"),
            json!({"kind":"move","a":12,"b":28,"revision":session["revision"]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &server,
            &owner,
            Method::POST,
            &format!("{path}/actions"),
            json!({"kind":"move","a":12,"b":44,"revision":session["revision"]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let revision = session["revision"].clone();
    session = ok(
        &server,
        &owner,
        Method::POST,
        &format!("{path}/actions"),
        json!({"kind":"move","a":12,"b":28,"revision":revision}),
    )
    .await;
    assert_eq!(session["game"]["board"][28], 1);
    assert_eq!(session["turn_user_id"], black.user.id);
    assert_eq!(
        call(
            &server,
            &owner,
            Method::POST,
            &format!("{path}/actions"),
            json!({"kind":"move","a":12,"b":28,"revision":revision})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // Fresh authenticated snapshots resume the same persisted state independently
    // of WebSocket presence or which participant happens to be the host.
    assert_eq!(
        ok(&server, &black, Method::GET, &path, Value::Null).await,
        session
    );
    let mut stale =
        accordserver::db::experiences::load(server.pool(), &space, session["id"].as_str().unwrap())
            .await
            .unwrap();
    ok(
        &server,
        &owner,
        Method::PATCH,
        &game,
        json!({"enabled":false}),
    )
    .await;
    assert!(
        accordserver::db::experiences::save(server.pool(), &mut stale)
            .await
            .is_err()
    );
    assert_eq!(
        ok(&server, &owner, Method::GET, &path, Value::Null).await["result"]["reason"],
        "disabled"
    );
    ok(
        &server,
        &owner,
        Method::PATCH,
        &game,
        json!({"enabled":true}),
    )
    .await;
    let lobby = ok(
        &server,
        &owner,
        Method::POST,
        &format!("{base}/arcade/sessions"),
        json!({"game_id":"chess","invite_only":true,"invited":[black.user.id]}),
    )
    .await;
    let private = format!("{base}/arcade/sessions/{}", lobby["id"].as_str().unwrap());
    assert_eq!(
        call(&server, &spectator, Method::GET, &private, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let listed = ok(
        &server,
        &spectator,
        Method::GET,
        &format!("{base}/arcade/sessions"),
        Value::Null,
    )
    .await;
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["id"] != lobby["id"]));
    // Starting requires two ready players. Player capacity is enforced while
    // spectators remain separate, and departing hosts transfer lobby ownership.
    let mut transfer = ok(
        &server,
        &owner,
        Method::POST,
        &format!("{base}/arcade/sessions"),
        json!({"game_id":"chess"}),
    )
    .await;
    let transfer_path = format!(
        "{base}/arcade/sessions/{}",
        transfer["id"].as_str().unwrap()
    );
    assert_eq!(
        call(
            &server,
            &owner,
            Method::POST,
            &format!("{transfer_path}/members"),
            json!({"operation":"start","revision":transfer["revision"]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    transfer = ok(
        &server,
        &black,
        Method::POST,
        &format!("{transfer_path}/members"),
        json!({"operation":"join","revision":transfer["revision"]}),
    )
    .await;
    assert_eq!(
        call(
            &server,
            &spectator,
            Method::POST,
            &format!("{transfer_path}/members"),
            json!({"operation":"join","revision":transfer["revision"]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    transfer = ok(
        &server,
        &spectator,
        Method::POST,
        &format!("{transfer_path}/members"),
        json!({"operation":"join","spectator":true,"revision":transfer["revision"]}),
    )
    .await;
    assert_eq!(
        call(
            &server,
            &spectator,
            Method::POST,
            &format!("{transfer_path}/members"),
            json!({"operation":"ready","ready":true,"revision":transfer["revision"]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    transfer = ok(
        &server,
        &owner,
        Method::POST,
        &format!("{transfer_path}/members"),
        json!({"operation":"leave","revision":transfer["revision"]}),
    )
    .await;
    assert_eq!(transfer["host_user_id"], black.user.id);
    transfer = ok(
        &server,
        &black,
        Method::POST,
        &format!("{transfer_path}/members"),
        json!({"operation":"leave","revision":transfer["revision"]}),
    )
    .await;
    transfer = ok(
        &server,
        &spectator,
        Method::POST,
        &format!("{transfer_path}/members"),
        json!({"operation":"leave","revision":transfer["revision"]}),
    )
    .await;
    assert_eq!(transfer["state"], "ended");

    // Checkmate is calculated by the server from legal moves, never client state.
    let mut mate = start_chess(&server, &owner, &black, &base).await;
    let mate_path = format!("{base}/arcade/sessions/{}", mate["id"].as_str().unwrap());
    for (user, a, b) in [
        (&owner, 13, 21),
        (&black, 52, 36),
        (&owner, 14, 30),
        (&black, 59, 31),
    ] {
        mate = ok(
            &server,
            user,
            Method::POST,
            &format!("{mate_path}/actions"),
            json!({"kind":"move","a":a,"b":b,"revision":mate["revision"]}),
        )
        .await;
    }
    assert_eq!(mate["state"], "ended");
    assert_eq!(mate["result"]["winner_user_id"], black.user.id);
    assert_eq!(mate["result"]["reason"], "checkmate");
    assert!(mate["turn_user_id"].is_null());

    // Maintenance records turn timeouts without any connected game client.
    let timed = start_chess(&server, &owner, &black, &base).await;
    let timed_id = timed["id"].as_str().unwrap();
    let mut expired = accordserver::db::experiences::load(server.pool(), &space, timed_id)
        .await
        .unwrap();
    expired.deadline = Some(chrono::Utc::now().timestamp() - 1);
    accordserver::db::experiences::save(server.pool(), &mut expired)
        .await
        .unwrap();
    accordserver::routes::experiences::maintenance(&server.state)
        .await
        .unwrap();
    let timed = accordserver::db::experiences::load(server.pool(), &space, timed_id)
        .await
        .unwrap();
    assert_eq!(timed.state, "ended");
    assert_eq!(timed.result.as_ref().unwrap()["reason"], "turn_timeout");
    assert_eq!(
        timed.result.as_ref().unwrap()["winner_user_id"],
        black.user.id
    );
    // Ended snapshots have bounded retention, with revision-guarded deletion.
    sqlx::query(&accordserver::db::q(
        "UPDATE experience_sessions SET updated_at = ? WHERE id = ?",
    ))
    .bind(chrono::Utc::now().timestamp() - 31 * 86400)
    .bind(timed_id)
    .execute(server.pool())
    .await
    .unwrap();
    accordserver::routes::experiences::maintenance(&server.state)
        .await
        .unwrap();
    assert!(
        accordserver::db::experiences::load(server.pool(), &space, timed_id)
            .await
            .is_err()
    );
    release.write().unwrap().status = "revoked".into();
    assert_eq!(
        call(
            &server,
            &owner,
            Method::GET,
            &format!("{game}/package"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ok(&server, &owner, Method::GET, &private, Value::Null).await["state"],
        "ended"
    );
    // Legacy direct executable uploads/routes cannot bypass the curated path.
    assert_eq!(
        call(
            &server,
            &owner,
            Method::POST,
            &format!("{base}/plugins"),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let pong_path = format!("{base}/experiences/pong");
    ok(
        &server,
        &owner,
        Method::PUT,
        &pong_path,
        json!({"version":"1.0.0"}),
    )
    .await;
    let mut pong = ok(
        &server,
        &owner,
        Method::POST,
        &format!("{base}/arcade/sessions"),
        json!({"game_id":"pong"}),
    )
    .await;
    let pong_session = format!("{base}/arcade/sessions/{}", pong["id"].as_str().unwrap());
    pong = ok(
        &server,
        &black,
        Method::POST,
        &format!("{pong_session}/members"),
        json!({"operation":"join","revision":pong["revision"]}),
    )
    .await;
    for user in [&owner, &black] {
        pong = ok(
            &server,
            user,
            Method::POST,
            &format!("{pong_session}/members"),
            json!({"operation":"ready","ready":true,"revision":pong["revision"]}),
        )
        .await;
    }
    pong = ok(
        &server,
        &owner,
        Method::POST,
        &format!("{pong_session}/members"),
        json!({"operation":"start","revision":pong["revision"]}),
    )
    .await;
    assert_eq!(pong["mode"], "real_time");
    let community = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = community.local_addr().unwrap();
    let community_app = server.router();
    let community_task = tokio::spawn(async move {
        axum::serve(community, community_app).await.unwrap();
    });
    let url = format!("ws://{address}{pong_session}/live");
    let (mut white_ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut black_ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    for (socket, user) in [(&mut white_ws, &owner), (&mut black_ws, &black)] {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"token":user.auth_header()}).to_string().into(),
            ))
            .await
            .unwrap();
    }
    white_ws
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"sequence":1,"kind":"input","a":300})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let observed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let message = black_ws.next().await.unwrap().unwrap();
            if let Ok(text) = message.into_text() {
                let frame: Value = serde_json::from_str(&text).unwrap();
                if frame["data"]["game"]["rects"][1] == 300
                    && frame["data"]["game"]["tick"].as_i64().unwrap_or(0) > 0
                {
                    break frame;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(observed["data"]["space_id"], space);
    // A disconnected player keeps the same slot on reconnect. The remaining
    // client sees a pause, then resumes from the server-owned state.
    let _ = black_ws.close(None).await;
    let mut paused =
        accordserver::db::experiences::load(server.pool(), &space, pong["id"].as_str().unwrap())
            .await
            .unwrap();
    paused
        .participants
        .iter_mut()
        .find(|p| p.user_id == black.user.id)
        .unwrap()
        .last_seen = chrono::Utc::now().timestamp() - 10;
    // Ticks may race this test setup; retry the same authoritative mutation.
    for _ in 0..20 {
        if accordserver::db::experiences::save(server.pool(), &mut paused)
            .await
            .is_ok()
        {
            break;
        }
        paused = accordserver::db::experiences::load(
            server.pool(),
            &space,
            pong["id"].as_str().unwrap(),
        )
        .await
        .unwrap();
        paused
            .participants
            .iter_mut()
            .find(|p| p.user_id == black.user.id)
            .unwrap()
            .last_seen = chrono::Utc::now().timestamp() - 10;
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let message = white_ws.next().await.unwrap().unwrap();
            if let Ok(text) = message.into_text() {
                let frame: Value = serde_json::from_str(&text).unwrap();
                if frame["data"]["game"]["paused"] == true {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    let (mut reconnected, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    reconnected
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"token":black.auth_header()}).to_string().into(),
        ))
        .await
        .unwrap();
    let resumed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let message = reconnected.next().await.unwrap().unwrap();
            if let Ok(text) = message.into_text() {
                let frame: Value = serde_json::from_str(&text).unwrap();
                if frame["data"]["game"]["paused"] == false {
                    break frame;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        resumed["data"]["participants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["user_id"] == black.user.id)
            .unwrap()["slot"],
        1
    );
    let _ = reconnected.close(None).await;
    ok(
        &server,
        &owner,
        Method::PATCH,
        &pong_path,
        json!({"enabled":false}),
    )
    .await;
    let ended = ok(&server, &black, Method::GET, &pong_session, Value::Null).await;
    assert_eq!(ended["state"], "ended");
    let _ = white_ws.close(None).await;
    let _ = black_ws.close(None).await;
    community_task.abort();
    task.abort();
}
