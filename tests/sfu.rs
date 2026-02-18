mod common;

use accordserver::sfu_client::SfuClient;

#[tokio::test]
async fn test_sfu_client_lifecycle() {
    let server = common::TestServer::new().await;
    let admin = server.create_admin_with_token("sfu_admin").await;
    let base_url = server.spawn().await;

    let client = SfuClient::new(
        base_url.clone(),
        "test-sfu-1".to_string(),
        "ws://test-sfu-1:4000".to_string(),
        "us-east".to_string(),
        100,
    )
    .with_auth_token(admin.token.clone());

    // Register
    client.register().await.expect("register should succeed");

    // Verify node appears in list
    let resp = reqwest::Client::new()
        .get(format!("{base_url}/api/v1/sfu/nodes"))
        .header("Authorization", admin.auth_header())
        .send()
        .await
        .expect("list request failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    let nodes = body["data"].as_array().expect("data should be an array");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["id"], "test-sfu-1");
    assert_eq!(nodes[0]["region"], "us-east");
    assert_eq!(nodes[0]["capacity"], 100);

    // Heartbeat
    client
        .heartbeat(42)
        .await
        .expect("heartbeat should succeed");

    // Verify load updated
    let resp = reqwest::Client::new()
        .get(format!("{base_url}/api/v1/sfu/nodes"))
        .header("Authorization", admin.auth_header())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let nodes = body["data"].as_array().unwrap();
    assert_eq!(nodes[0]["current_load"], 42);

    // Deregister
    client
        .deregister()
        .await
        .expect("deregister should succeed");

    // Verify node is gone
    let resp = reqwest::Client::new()
        .get(format!("{base_url}/api/v1/sfu/nodes"))
        .header("Authorization", admin.auth_header())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let nodes = body["data"].as_array().unwrap();
    assert!(nodes.is_empty());
}

#[tokio::test]
async fn test_sfu_client_re_register() {
    let server = common::TestServer::new().await;
    let admin = server.create_admin_with_token("sfu_admin2").await;
    let base_url = server.spawn().await;

    let client = SfuClient::new(
        base_url,
        "test-sfu-2".to_string(),
        "ws://test-sfu-2:4000".to_string(),
        "eu-west".to_string(),
        50,
    )
    .with_auth_token(admin.token);

    // Register twice (upsert) should succeed
    client.register().await.expect("first register");
    client.register().await.expect("second register (upsert)");

    // Deregister
    client.deregister().await.expect("deregister");
}
