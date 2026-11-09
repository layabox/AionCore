//! Authentication and validation for explicit native catalog discovery.
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{body_json, build_app, json_with_token, setup_and_login};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn claude_catalog_refresh_requires_authentication() {
    let (app, _) = build_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/agents/2d23ff1c/refresh-config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
}

#[tokio::test]
async fn claude_catalog_refresh_requires_csrf() {
    let (mut app, services) = build_app().await;
    let (token, _) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/agents/2d23ff1c/refresh-config")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn claude_catalog_refresh_rejects_unknown_and_other_builtin_targets() {
    let (mut app, services) = build_app().await;
    let (token, csrf) = setup_and_login(&mut app, &services, "admin", "StrongP@ss1").await;
    let codex = services.agent_registry.find_builtin_by_backend("codex").await.unwrap();
    for (id, expected, message) in [
        ("missing", StatusCode::NOT_FOUND, "Agent not found"),
        (
            codex.id.as_str(),
            StatusCode::BAD_REQUEST,
            "only for the builtin Claude",
        ),
    ] {
        let request = json_with_token(
            "POST",
            &format!("/api/agents/{id}/refresh-config"),
            json!({}),
            &token,
            &csrf,
        );
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
        assert!(body_json(response).await["error"].as_str().unwrap().contains(message));
    }
}

#[tokio::test]
async fn claude_catalog_refresh_does_not_reveal_or_spawn_another_users_agent() {
    let (mut app, services) = build_app().await;
    setup_and_login(&mut app, &services, "alice", "StrongP@ss1").await;
    let alice = services.user_repo.find_by_username("alice").await.unwrap().unwrap();
    let claude = services.agent_registry.find_builtin_by_backend("claude").await.unwrap();
    sqlx::query("INSERT INTO agent_metadata (id, agent_id, user_id, name, backend, agent_type, agent_source, enabled, command, args, env, sort_order, created_at, updated_at) SELECT 'private-claude', 'private-claude', ?, name, backend, agent_type, 'custom', enabled, command, args, env, sort_order, created_at, updated_at FROM agent_metadata WHERE id = ?")
        .bind(&alice.id).bind(&claude.id).execute(services.database.pool()).await.unwrap();
    let (token, csrf) = setup_and_login(&mut app, &services, "bob", "StrongP@ss1").await;
    let response = app
        .oneshot(json_with_token(
            "POST",
            "/api/agents/private-claude/refresh-config",
            json!({}),
            &token,
            &csrf,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        body_json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("Agent not found")
    );
}
