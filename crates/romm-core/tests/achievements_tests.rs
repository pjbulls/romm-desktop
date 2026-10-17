//! RetroAchievements through the real HTTP client against a fake RomM.
//! Payload shapes follow RomM's `RomSchema`, `UserSchema` and `RAUserProgression`.

use romm_core::achievements::{build, RaState};
use romm_core::api::RommClient;
use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> RommClient {
    RommClient::new(&server.uri(), Some("rmm_t".into()), false).unwrap()
}

/// A ROM response with plenty of unrelated fields, as the real endpoint returns.
fn rom_response() -> Value {
    json!({
        "id": 7, "name": "Sonic the Hedgehog", "fs_name": "sonic.md", "platform_id": 2, "platform_slug": "genesis",
        "ra_id": 1, "igdb_id": 99, "files": [], "rom_user": {"is_main_sibling": true}, "sibling_roms": [],
        "merged_screenshots": ["/a.png"], "some_future_field": {"nested": [1, 2, 3]},
        "merged_ra_metadata": {
            "first_release_date": 1, "genres": ["Platformer"], "companies": [], "hash_match": true,
            "achievements": [
                {"ra_id": 9001, "title": "Ring Collector", "description": "Get 100 rings", "points": 5, "num_awarded": 10,
                 "num_awarded_hardcore": 5, "badge_id": "10001", "badge_url": "https://media/x.png", "badge_url_lock": "https://media/x_lock.png",
                 "badge_path": "/assets/romm/resources/roms/2/7/retroachievements/badges/10001.png",
                 "badge_path_lock": "/assets/romm/resources/roms/2/7/retroachievements/badges/10001_lock.png",
                 "display_order": 1, "type": "progression"},
                {"ra_id": 9002, "title": "Green Hill Clear", "description": "Finish act 3", "points": 10, "badge_id": "10002",
                 "badge_path": "/assets/romm/resources/roms/2/7/retroachievements/badges/10002.png",
                 "badge_path_lock": "/assets/romm/resources/roms/2/7/retroachievements/badges/10002_lock.png",
                 "display_order": 2, "type": "win_condition"}
            ]
        }
    })
}

fn me_response() -> Value {
    json!({
        "id": 42, "username": "pj", "email": null, "enabled": true, "role": "admin", "oauth_scopes": ["me.read"],
        "avatar_path": "", "ra_username": "pj_ra", "ui_settings": {"x": 1},
        "ra_progression": {"total": 1, "results": [
            {"rom_ra_id": 1, "max_possible": 15, "num_awarded": 1, "num_awarded_hardcore": 1,
             "most_recent_awarded_date": "2026-03-04 05:06:07", "highest_award_kind": null,
             "earned_achievements": [{"id": "10002", "date": "2026-03-04 05:06:07", "date_hardcore": "2026-03-04 05:06:07"}]}
        ]}
    })
}

#[tokio::test]
async fn client_reads_both_responses_and_the_merge_marks_the_right_achievement() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/roms/7")).and(header("authorization", "Bearer rmm_t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(rom_response())).mount(&server).await;
    Mock::given(method("GET")).and(path("/api/users/me")).and(header("authorization", "Bearer rmm_t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(me_response())).mount(&server).await;

    let c = client(&server);
    let (rom, me) = (c.rom_ra(7).await.unwrap(), c.me().await.unwrap());
    assert_eq!(me.id, 42);

    let v = build(&rom, &me);
    assert_eq!(v.state, RaState::Ready);
    assert_eq!((v.total, v.earned, v.earned_hardcore), (2, 1, 1));
    assert_eq!((v.points_total, v.points_earned), (15, 10));
    assert!(!v.achievements[0].earned, "Ring Collector (badge 10001) was not earned");
    assert!(v.achievements[1].earned, "Green Hill Clear (badge 10002) was earned");
    assert_eq!(v.achievements[1].kind, "win_condition");
    assert!(v.achievements[1].badge.as_deref().unwrap().ends_with("/10002.png"));
    assert!(v.achievements[0].badge.as_deref().unwrap().ends_with("/10001_lock.png"));
    assert_eq!(v.last_earned.as_deref(), Some("2026-03-04T05:06:07Z"));
}

#[tokio::test]
async fn a_user_with_no_progression_yet_deserializes() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/users/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1, "username": "u", "ra_username": null, "ra_progression": null})))
        .mount(&server).await;
    let me = client(&server).me().await.unwrap();
    assert!(me.ra_username.is_none() && me.ra_progression.is_none());
}

#[tokio::test]
async fn refresh_posts_the_incremental_flag_to_the_users_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/users/42/ra/refresh")).and(header("authorization", "Bearer rmm_t"))
        .and(body_json(json!({"incremental": true})))
        .respond_with(ResponseTemplate::new(200)).expect(1).mount(&server).await;
    client(&server).refresh_ra(42, true).await.unwrap();

    let full = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/users/42/ra/refresh")).and(body_json(json!({"incremental": false})))
        .respond_with(ResponseTemplate::new(200)).expect(1).mount(&full).await;
    client(&full).refresh_ra(42, false).await.unwrap();
}

#[tokio::test]
async fn refresh_without_the_me_write_scope_surfaces_403() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/users/42/ra/refresh"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"detail": "Insufficient scope"}))).mount(&server).await;
    let err = client(&server).refresh_ra(42, true).await.unwrap_err();
    assert_eq!(err.status(), Some(403));
}

#[tokio::test]
async fn refresh_with_no_ra_username_surfaces_404_and_the_reason() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/users/42/ra/refresh"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"detail": "User does not have a RetroAchievements username set"})))
        .mount(&server).await;
    let err = client(&server).refresh_ra(42, true).await.unwrap_err();
    assert_eq!(err.status(), Some(404));
    assert!(err.to_string().contains("RetroAchievements username"));
}
