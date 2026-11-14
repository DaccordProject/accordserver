//! Instance-admin device bans: fingerprint recording on sign-in, refusal of
//! banned devices at register/login/session check-in, and the admin API.

mod common;

use accordserver::db;
use common::{
    authenticated_json_request, authenticated_request, json_request, TestServer, TestUser,
};
use http::{Method, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

const FP_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FP_B: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const FP_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

async fn call(server: &TestServer, req: http::Request<axum::body::Body>) -> (StatusCode, Value) {
    let response = server.router().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

async fn register(server: &TestServer, username: &str, fp: Option<&str>) -> (StatusCode, Value) {
    let mut body = json!({ "username": username, "password": "securepassword123" });
    if let Some(fp) = fp {
        body["device_fingerprint"] = json!(fp);
    }
    call(
        server,
        json_request(Method::POST, "/api/v1/auth/register", &body),
    )
    .await
}

async fn login(server: &TestServer, username: &str, fp: Option<&str>) -> (StatusCode, Value) {
    let mut body = json!({ "username": username, "password": "securepassword123" });
    if let Some(fp) = fp {
        body["device_fingerprint"] = json!(fp);
    }
    call(
        server,
        json_request(Method::POST, "/api/v1/auth/login", &body),
    )
    .await
}

async fn ban_devices(
    server: &TestServer,
    admin: &TestUser,
    user_id: &str,
    body: Value,
) -> (StatusCode, Value) {
    call(
        server,
        authenticated_json_request(
            Method::POST,
            &format!("/api/v1/admin/users/{user_id}/device-bans"),
            &admin.auth_header(),
            &body,
        ),
    )
    .await
}

async fn put_device(server: &TestServer, auth_header: &str, body: Value) -> (StatusCode, Value) {
    call(
        server,
        authenticated_json_request(Method::PUT, "/api/v1/users/@me/device", auth_header, &body),
    )
    .await
}

#[tokio::test]
async fn register_validates_and_records_fingerprint() {
    let server = TestServer::new().await;

    let (status, body) = register(&server, "malformed", Some("not-a-fingerprint")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(message(&body), "invalid device_fingerprint");
    // Uppercase hex is not the canonical form.
    let (status, _) = register(&server, "upper", Some(&FP_A.to_uppercase())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = register(&server, "nofp", None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = register(&server, "withfp", Some(FP_A)).await;
    assert_eq!(status, StatusCode::OK);
    let user_id = body["data"]["user"]["id"].as_str().unwrap();
    assert_eq!(
        db::devices::list_user_fingerprints(server.pool(), user_id)
            .await
            .unwrap(),
        vec![FP_A.to_string()]
    );
}

#[tokio::test]
async fn banned_device_cannot_register_or_sign_in_to_any_account() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;

    let (status, _) = register(&server, "other", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = register(&server, "evader", None).await;
    assert_eq!(status, StatusCode::OK);
    let evader_id = body["data"]["user"]["id"].as_str().unwrap().to_string();

    // Login records the device.
    let (status, body) = login(&server, "evader", Some(FP_A)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["token"].is_string());
    assert_eq!(
        db::devices::list_user_fingerprints(server.pool(), &evader_id)
            .await
            .unwrap(),
        vec![FP_A.to_string()]
    );

    let (status, body) =
        ban_devices(&server, &admin, &evader_id, json!({ "reason": "spam" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["banned"], 1);
    let ban = &body["data"]["bans"][0];
    assert_eq!(ban["fingerprint"], FP_A);
    assert_eq!(ban["user_id"], evader_id.as_str());
    assert_eq!(ban["username"], "evader");
    assert_eq!(ban["reason"], "spam");
    assert_eq!(ban["banned_by"], admin.user.id.as_str());
    assert!(ban["id"].is_string() && ban["created_at"].is_string());

    let (status, body) = register(&server, "fresh", Some(FP_A)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        message(&body),
        "registration is not available from this device"
    );

    for username in ["evader", "other"] {
        let (status, body) = login(&server, username, Some(FP_A)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(message(&body), "sign-in is not available from this device");
    }
    // The ban is device-level: another device still signs in.
    let (status, _) = login(&server, "other", Some(FP_B)).await;
    assert_eq!(status, StatusCode::OK);

    // The MFA step validates and checks the device before the ticket.
    let (status, _) = call(
        &server,
        json_request(
            Method::POST,
            "/api/v1/auth/login/mfa",
            &json!({ "ticket": "x", "code": "123456", "device_fingerprint": FP_A }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &server,
        json_request(
            Method::POST,
            "/api/v1/auth/login/mfa",
            &json!({ "ticket": "x", "code": "123456", "device_fingerprint": "zz" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Banning again creates nothing new.
    let (status, body) = ban_devices(&server, &admin, &evader_id, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["banned"], 0);
    assert_eq!(body["data"]["bans"], json!([]));
}

#[tokio::test]
async fn session_device_check_in_records_and_revokes_banned_device() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let user = server.create_user_with_token("user").await;

    let (status, _) = put_device(&server, &user.auth_header(), json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = put_device(
        &server,
        &user.auth_header(),
        json!({ "device_fingerprint": "abc" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(message(&body), "invalid device_fingerprint");

    let (status, body) = put_device(
        &server,
        &user.auth_header(),
        json!({ "device_fingerprint": FP_C }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);
    assert_eq!(
        db::devices::list_user_fingerprints(server.pool(), &user.user.id)
            .await
            .unwrap(),
        vec![FP_C.to_string()]
    );

    let (status, _) = ban_devices(&server, &admin, &user.user.id, json!({})).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = put_device(
        &server,
        &user.auth_header(),
        json!({ "device_fingerprint": FP_C }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(message(&body), "this device is banned");

    let (status, _) = call(
        &server,
        authenticated_request(Method::GET, "/api/v1/users/@me", &user.auth_header()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bots_check_in_as_a_no_op() {
    let server = TestServer::new().await;
    let (_owner, bot) = server.create_bot_with_token("owner", "app").await;
    let (status, _) = put_device(
        &server,
        &bot.auth_header(),
        json!({ "device_fingerprint": FP_A }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        db::devices::list_user_fingerprints(server.pool(), &bot.user.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn admin_device_ban_guards() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let other_admin = server.create_admin_with_token("admin2").await;
    let user = server.create_user_with_token("user").await;
    let target = server.create_user_with_token("target").await;

    let (status, body) = ban_devices(&server, &admin, &admin.user.id, json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(message(&body), "you cannot ban your own devices");

    let (status, body) = ban_devices(&server, &admin, &other_admin.user.id, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(message(&body), "cannot ban another administrator's devices");

    let (status, _) = ban_devices(&server, &user, &target.user.id, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = ban_devices(&server, &admin, "999999", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = ban_devices(
        &server,
        &admin,
        &target.user.id,
        json!({ "reason": "x".repeat(513) }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    for path in ["/api/v1/admin/device-bans", "/api/v1/admin/device-bans/1"] {
        let method = if path.ends_with("/1") {
            Method::DELETE
        } else {
            Method::GET
        };
        let (status, _) = call(
            &server,
            authenticated_request(method, path, &user.auth_header()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
}

#[tokio::test]
async fn admin_lists_and_deletes_device_bans_and_bans_outlive_account() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let target = server.create_user_with_token("target").await;
    for fp in [FP_A, FP_B, FP_C] {
        db::devices::record_device(
            server.pool(),
            &target.user.id,
            fp,
            server.state.db_is_postgres,
        )
        .await
        .unwrap();
    }

    let (status, body) = ban_devices(&server, &admin, &target.user.id, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["banned"], 3);
    assert!(body["data"]["bans"][0]["reason"].is_null());

    let list = |query: &'static str| {
        let header = admin.auth_header();
        let server = &server;
        async move {
            call(
                server,
                authenticated_request(
                    Method::GET,
                    &format!("/api/v1/admin/device-bans{query}"),
                    &header,
                ),
            )
            .await
        }
    };

    let (status, body) = list("").await;
    assert_eq!(status, StatusCode::OK);
    let all = body["data"].as_array().unwrap().clone();
    assert_eq!(all.len(), 3);
    let ids: Vec<&str> = all.iter().map(|b| b["id"].as_str().unwrap()).collect();
    let mut newest_first = ids.clone();
    newest_first.sort_by(|a, b| b.cmp(a));
    assert_eq!(ids, newest_first);

    let (_, page) = list("?limit=2").await;
    assert_eq!(page["data"].as_array().unwrap().len(), 2);
    let after = page["data"][1]["id"].as_str().unwrap().to_string();
    let (_, rest) = call(
        &server,
        authenticated_request(
            Method::GET,
            &format!("/api/v1/admin/device-bans?after={after}&limit=2"),
            &admin.auth_header(),
        ),
    )
    .await;
    assert_eq!(rest["data"].as_array().unwrap().len(), 1);
    assert_eq!(rest["data"][0]["id"], all[2]["id"]);

    // Deleting the account drops its device records but keeps the bans.
    let (status, _) = call(
        &server,
        authenticated_request(
            Method::DELETE,
            &format!("/api/v1/admin/users/{}", target.user.id),
            &admin.auth_header(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        db::devices::list_user_fingerprints(server.pool(), &target.user.id)
            .await
            .unwrap()
            .is_empty()
    );
    let (_, body) = list("").await;
    assert_eq!(body["data"].as_array().unwrap().len(), 3);
    assert!(db::devices::is_fingerprint_banned(server.pool(), FP_B)
        .await
        .unwrap());

    let ban_id = all[0]["id"].as_str().unwrap().to_string();
    let delete = |id: String| {
        let header = admin.auth_header();
        let server = &server;
        async move {
            call(
                server,
                authenticated_request(
                    Method::DELETE,
                    &format!("/api/v1/admin/device-bans/{id}"),
                    &header,
                ),
            )
            .await
        }
    };
    let (status, body) = delete(ban_id.clone()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);
    let (status, _) = delete(ban_id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, body) = list("").await;
    assert_eq!(body["data"].as_array().unwrap().len(), 2);
}
