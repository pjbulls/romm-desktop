//! End-to-end tests of the sync engine and play sessions against a mock RomM server.
//!
//! What a game's save is, and the bytes that travel, are checked against argosy-sigil's reference
//! output in `unit_tests.rs`; these tests check the protocol around it: what is negotiated, what is
//! uploaded and restored, and what is remembered afterwards.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use common::*;
use romm_core::api::RommClient;
use romm_core::paths::{Os, PathCtx};
use romm_core::profiles;
use romm_core::retroarch;
use romm_core::saves;
use romm_core::state::{save_key, LocalRom, State};
use romm_core::sync::{saves_overview, ConflictPolicy, EmuEnv, SaveStatus, Syncer};
use romm_core::unit::{self, Identity, Route};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const DEVICE: &str = "dev-1";
const SERVER_TIME: &str = "2026-01-02T03:04:05Z";

fn retroarch_env(root: &Path, core: &str) -> EmuEnv {
    EmuEnv {
        profile: profiles::get("retroarch").unwrap(),
        data_root: root.to_path_buf(),
        retroarch: Some(retroarch::layout(root, Os::Linux, &PathCtx::default())),
        core: Some(core.into()),
    }
}

fn standalone_env(id: &str, root: &Path) -> EmuEnv {
    EmuEnv { profile: profiles::get(id).unwrap(), data_root: root.to_path_buf(), retroarch: None, core: None }
}

/// A ROM that isn't on disk: fine for emulators whose saves are found by file name.
fn rom(stem: &str, platform: &str) -> LocalRom {
    LocalRom {
        rom_id: 7,
        name: stem.into(),
        platform_slug: platform.into(),
        path: PathBuf::from(format!("/roms/{stem}")),
        launch_path: PathBuf::from(format!("/roms/{stem}.rom")),
        stem: stem.into(),
        ..Default::default()
    }
}

/// A ROM that exists (empty), named with its id the way RomM libraries often are, which is all
/// Sigil's filename fallback needs to identify the game.
fn rom_file(dir: &Path, file: &str, platform: &str) -> LocalRom {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(file);
    std::fs::write(&p, b"").unwrap();
    LocalRom {
        rom_id: 7,
        name: file.into(),
        platform_slug: platform.into(),
        path: p.clone(),
        stem: p.file_stem().unwrap().to_string_lossy().into_owned(),
        launch_path: p,
        ..Default::default()
    }
}

/// A ROM that is a real disc image, so the game is identified from inside it (not from its name).
fn rom_disc(dir: &Path, file: &str, platform: &str, image: &[u8]) -> LocalRom {
    let r = rom_file(dir, file, platform);
    std::fs::write(&r.launch_path, image).unwrap();
    r
}

/// What Sigil would have read from the ROM, as if the game had been synced before. For systems
/// whose ids live in the disc header (GameCube, 3DS) and so can't be faked with a file name.
fn with_identity(st: &mut State, rom: &LocalRom, id: Identity) {
    let mut r = rom.clone();
    r.identity = Some(Identity { source: rom.launch_path.to_string_lossy().into_owned(), ..id });
    st.roms.insert(rom.rom_id, r);
}

fn save_json(id: i64, name: &str) -> Value {
    json!({"id": id, "rom_id": 7, "file_name": name, "file_size_bytes": 1, "emulator": "x", "slot": null, "content_hash": null})
}

/// A save as the server answers an upload: it has computed its own hash and timestamp.
fn stored_json(id: i64, name: &str, hash: &str) -> Value {
    json!({"id": id, "rom_id": 7, "file_name": name, "file_size_bytes": 1, "emulator": "x", "slot": "autosave", "content_hash": hash, "updated_at": SERVER_TIME})
}

fn op(action: &str, save_id: Option<i64>, name: &str) -> Value {
    json!({"action": action, "rom_id": 7, "save_id": save_id, "file_name": name, "slot": "autosave", "emulator": null})
}

async fn mock_negotiate(server: &MockServer, ops: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/api/sync/negotiate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"session_id": 42, "operations": ops})))
        .mount(server)
        .await;
}

/// Mount a negotiate answer for the next request only; later ones fall through to whatever is mounted after.
async fn mock_negotiate_once(server: &MockServer, ops: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path("/api/sync/negotiate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"session_id": 42, "operations": ops})))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

async fn mock_complete(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/sync/sessions/\d+/complete$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(server)
        .await;
}

async fn reqs(server: &MockServer, m: &str, p: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == m && r.url.path() == p)
        .collect()
}

fn query(r: &Request) -> HashMap<String, String> {
    r.url.query_pairs().into_owned().collect()
}

fn client(server: &MockServer) -> RommClient {
    RommClient::new(&server.uri(), Some("rmm_test".into()), false).unwrap()
}

fn syncer<'a>(c: &'a RommClient, st: &'a mut State, bak: &Path, policy: ConflictPolicy) -> Syncer<'a> {
    Syncer { client: c, device_id: DEVICE, state: st, backup_dir: bak.to_path_buf(), policy }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn find_file_containing(dir: &Path, needle: &[u8]) -> bool {
    walk(dir).iter().any(|p| std::fs::read(p).map(|b| b == needle).unwrap_or(false))
}

fn body_text(r: &Request) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

fn psp_route(root: &Path) -> Route {
    Route { layout: "ppsspp_standalone".into(), platform: "psp".into(), root: root.to_path_buf(), options: vec![] }
}

// ============================================================================================
// RetroArch: one raw file per game (cores Argosy has no layout for)
// ============================================================================================

#[tokio::test]
async fn fresh_device_downloads_server_save() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("download", Some(99), "Zelda.srm")]).await;
    Mock::given(method("GET"))
        .and(path("/api/saves/99/content"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"SERVERSAVE".to_vec()))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"), "snes9x");
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth)
        .sync_rom(&rom("Zelda", "snes"), &env, None)
        .await
        .unwrap();

    assert_eq!(std::fs::read(t.path().join("ra/saves/Zelda.srm")).unwrap(), b"SERVERSAVE");
    assert_eq!(rep.downloaded, ["Zelda.srm"]);
    assert!(rep.is_clean());

    let neg = &reqs(&server, "POST", "/api/sync/negotiate").await[0];
    let body: Value = neg.body_json().unwrap();
    assert_eq!(body["device_id"], DEVICE);
    assert_eq!(body["rom_ids"], json!([7]));
    // Argosy sends no emulator filter, and a filter would hide saves other clients wrote under
    // another label (`ppsspp_gold`, a libretro core's name): there must be none.
    assert!(body.get("emulators").is_none(), "{body}");
    assert_eq!(body["saves"], json!([]));
    assert_eq!(neg.headers.get("authorization").unwrap().to_str().unwrap(), "Bearer rmm_test");

    let done: Value = reqs(&server, "POST", "/api/sync/sessions/42/complete").await[0].body_json().unwrap();
    assert_eq!(done["operations_completed"], 1);
    assert_eq!(done["operations_failed"], 0);
}

#[tokio::test]
async fn local_save_is_negotiated_and_uploaded_labelled_with_the_core() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("upload", None, "Zelda.srm")]).await;
    let md5 = saves::md5_bytes(b"LOCAL");
    Mock::given(method("POST"))
        .and(path("/api/saves"))
        .respond_with(ResponseTemplate::new(201).set_body_json(stored_json(11, "Zelda.srm", &md5)))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"), "snes9x");
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL").unwrap();

    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth)
        .sync_rom(&rom("Zelda", "snes"), &env, None)
        .await
        .unwrap();
    assert_eq!(rep.uploaded, ["Zelda.srm"]);

    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"][0]["content_hash"], md5.as_str());
    assert_eq!(neg["saves"][0]["slot"], "autosave");
    // Argosy labels a RetroArch save with the libretro core's name, not "retroarch".
    assert_eq!(neg["saves"][0]["emulator"], "snes9x");
    assert_eq!(neg["saves"][0]["file_name"], "Zelda.srm");

    let up = &reqs(&server, "POST", "/api/saves").await[0];
    let q = query(up);
    assert_eq!(q["rom_id"], "7");
    assert_eq!(q["slot"], "autosave");
    assert_eq!(q["emulator"], "snes9x");
    assert_eq!(q["device_id"], DEVICE);
    assert_eq!(q["session_id"], "42");
    assert_eq!(q["content_hash"], md5);
    assert_eq!(q["overwrite"], "false");
    let body = body_text(up);
    assert!(body.contains("name=\"saveFile\"") && body.contains("LOCAL"), "{body}");

    let rec = &st.saves[&save_key(7, "snes9x")];
    assert_eq!(rec.server_save_id, Some(11));
    assert_eq!(rec.last_hash.as_deref(), Some(md5.as_str()));
    assert_eq!(rec.server_updated_at.as_deref(), Some(SERVER_TIME));
}

#[tokio::test]
async fn a_sav_file_is_found_and_uploaded_under_its_own_extension() {
    // Argosy looks for <stem>.srm, then <stem>.sav, and uploads whichever it finds as it is.
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("upload", None, "Zelda.sav")]).await;
    Mock::given(method("POST"))
        .and(path("/api/saves"))
        .respond_with(ResponseTemplate::new(201).set_body_json(save_json(11, "Zelda.sav")))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"), "snes9x");
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.sav"), b"SAV").unwrap();
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&rom("Zelda", "snes"), &env, None).await.unwrap();
    assert_eq!(rep.uploaded, ["Zelda.sav"]);
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"][0]["file_name"], "Zelda.sav");
}

#[tokio::test]
async fn a_save_unchanged_since_the_last_sync_is_reported_with_the_servers_hash_and_time() {
    // While nothing changed, the server's own hash and timestamp go back to it (as Argosy does),
    // so a save that was merely restored or touched can never look newer than the server's copy.
    let server = MockServer::start().await;
    let md5 = saves::md5_bytes(b"LOCAL");
    mock_negotiate_once(&server, vec![op("upload", None, "Zelda.srm")]).await;
    mock_negotiate(&server, vec![op("no_op", Some(11), "Zelda.srm")]).await;
    Mock::given(method("POST"))
        .and(path("/api/saves"))
        .respond_with(ResponseTemplate::new(201).set_body_json(stored_json(11, "Zelda.srm", &md5)))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"), "snes9x");
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL").unwrap();
    let c = client(&server);
    let mut st = State::default();
    let bak = t.path().join("bak");
    syncer(&c, &mut st, &bak, ConflictPolicy::KeepBoth).sync_rom(&rom("Zelda", "snes"), &env, None).await.unwrap();
    let rep = syncer(&c, &mut st, &bak, ConflictPolicy::KeepBoth).sync_rom(&rom("Zelda", "snes"), &env, None).await.unwrap();
    assert_eq!(rep.unchanged, 1);

    let second: Value = reqs(&server, "POST", "/api/sync/negotiate").await[1].body_json().unwrap();
    assert_eq!(second["saves"][0]["content_hash"], md5.as_str());
    assert_eq!(second["saves"][0]["updated_at"], SERVER_TIME);
    assert_eq!(reqs(&server, "POST", "/api/saves").await.len(), 1, "no second upload");
}

#[tokio::test]
async fn a_unit_from_another_client_is_never_written_into_a_plain_save_file() {
    // Argosy uploaded a zip (a core with its own layout, e.g. Game Boy with a clock). This core
    // keeps a plain file, so writing the zip there would corrupt the save: refuse, change nothing.
    let t = tempfile::tempdir().unwrap();
    let unit_zip = {
        let src = t.path().join("src");
        psp_memory_stick(&src);
        let id = Identity { platform: "psp".into(), title_id: "ULUS10064".into(), save_id: "ULUS10064".into(), ..Default::default() };
        unit::collect(&psp_route(&src.join("PSP")), &id, "x.iso", None).unwrap().unwrap().data
    };
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("download", Some(99), "Zelda.zip")]).await;
    Mock::given(method("GET")).and(path("/api/saves/99/content")).respond_with(ResponseTemplate::new(200).set_body_bytes(unit_zip)).mount(&server).await;
    mock_complete(&server).await;

    let env = retroarch_env(&t.path().join("ra"), "snes9x");
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"MINE").unwrap();
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::ServerWins).sync_rom(&rom("Zelda", "snes"), &env, None).await.unwrap();
    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    assert!(rep.downloaded.is_empty());
    assert_eq!(std::fs::read(t.path().join("ra/saves/Zelda.srm")).unwrap(), b"MINE");
}

#[tokio::test]
async fn conflict_keep_both_archives_local_then_adopts_server() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("conflict", Some(5), "Zelda.srm")]).await;
    Mock::given(method("POST"))
        .and(path("/api/saves"))
        .respond_with(ResponseTemplate::new(201).set_body_json(save_json(12, "archived")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/saves/5/content"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"SERVER".to_vec()))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"), "snes9x");
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL-PROGRESS").unwrap();

    let c = client(&server);
    let mut st = State::default();
    let bak = t.path().join("bak");
    let rep = syncer(&c, &mut st, &bak, ConflictPolicy::KeepBoth).sync_rom(&rom("Zelda", "snes"), &env, None).await.unwrap();

    // The local copy went to the server as an archival (slot-less) save...
    let up = &reqs(&server, "POST", "/api/saves").await[0];
    assert!(!query(up).contains_key("slot"), "archival saves must not claim the slot");
    let body = body_text(up);
    assert!(body.contains("conflict-") && body.contains("LOCAL-PROGRESS"), "{body}");
    // ...and was also backed up on disk before being replaced by the server's.
    assert_eq!(std::fs::read(t.path().join("ra/saves/Zelda.srm")).unwrap(), b"SERVER");
    assert!(find_file_containing(&bak, b"LOCAL-PROGRESS"), "local save must be backed up");
    assert_eq!(rep.archived.len(), 1);
    assert_eq!(rep.downloaded, ["Zelda.srm"]);
    assert!(rep.is_clean());
}

#[tokio::test]
async fn conflict_ask_changes_nothing() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("conflict", Some(5), "Zelda.srm")]).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"), "snes9x");
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL").unwrap();

    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::Ask).sync_rom(&rom("Zelda", "snes"), &env, None).await.unwrap();
    assert_eq!(rep.conflicts.len(), 1);
    assert_eq!(std::fs::read(t.path().join("ra/saves/Zelda.srm")).unwrap(), b"LOCAL");
    assert!(reqs(&server, "POST", "/api/saves").await.is_empty());
    assert!(reqs(&server, "GET", "/api/saves/5/content").await.is_empty());
}

#[tokio::test]
async fn conflict_device_wins_replaces_server_save() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("conflict", Some(5), "Zelda.srm")]).await;
    Mock::given(method("PUT"))
        .and(path("/api/saves/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(save_json(5, "Zelda.srm")))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"), "snes9x");
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL").unwrap();
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::DeviceWins).sync_rom(&rom("Zelda", "snes"), &env, None).await.unwrap();
    assert_eq!(rep.uploaded, ["Zelda.srm"]);
    let put = &reqs(&server, "PUT", "/api/saves/5").await[0];
    assert_eq!(query(put)["content_hash"], saves::md5_bytes(b"LOCAL"));
}

#[tokio::test]
async fn upload_409_is_reported_as_conflict_not_failure() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("upload", None, "Zelda.srm")]).await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(409).set_body_string("slot already filled")).mount(&server).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let env = retroarch_env(&t.path().join("ra"), "snes9x");
    std::fs::create_dir_all(t.path().join("ra/saves")).unwrap();
    std::fs::write(t.path().join("ra/saves/Zelda.srm"), b"LOCAL").unwrap();
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&rom("Zelda", "snes"), &env, None).await.unwrap();
    assert_eq!(rep.conflicts.len(), 1);
    assert!(rep.failed.is_empty());
    assert!(rep.uploaded.is_empty());
}

// ============================================================================================
// RetroArch cores Argosy routes as units (Game Boy)
// ============================================================================================

#[tokio::test]
async fn a_game_boy_cart_with_a_clock_travels_as_a_zip_and_without_one_as_the_plain_srm() {
    for (features, expect_name, expect_zip) in [(1u32, "Pokemon Crystal.zip", true), (0, "Pokemon Crystal.srm", false)] {
        let server = MockServer::start().await;
        mock_negotiate(&server, vec![op("upload", None, expect_name)]).await;
        Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(201).set_body_json(save_json(11, expect_name))).mount(&server).await;
        mock_complete(&server).await;

        let t = tempfile::tempdir().unwrap();
        let env = retroarch_env(&t.path().join("ra"), "gambatte");
        write(&t.path().join("ra"), "saves/Pokemon Crystal.srm", &[b'S'; 64]);
        write(&t.path().join("ra"), "saves/Pokemon Crystal.rtc", &[b'R'; 16]);
        let r = rom("Pokemon Crystal", "gbc");
        let mut st = State::default();
        with_identity(&mut st, &r, Identity { platform: "gbc".into(), features, ..Default::default() });
        let c = client(&server);
        let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&r, &env, None).await.unwrap();

        assert_eq!(rep.uploaded, [expect_name], "features={features}: {rep:?}");
        let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
        assert_eq!(neg["saves"][0]["emulator"], "gambatte");
        let body = body_text(&reqs(&server, "POST", "/api/saves").await[0]);
        if expect_zip {
            assert!(body.contains("save.sram"), "a zip unit, members named as Argosy's: {body}");
        } else {
            // Exactly the file RetroArch wrote, byte for byte, so RomM's web player can read it too.
            assert_eq!(neg["saves"][0]["content_hash"], saves::md5_bytes(&[b'S'; 64]).as_str());
        }
    }
}

// ============================================================================================
// Shared volumes whose saves carry no game id (Dreamcast VMU; Sega CD and Saturn work the same)
// ============================================================================================

fn play_session() -> romm_core::api::types::PlaySession {
    romm_core::api::types::PlaySession { rom_id: 7, save_slot: None, start_time: "2026-01-01T00:00:00Z".into(), end_time: "2026-01-01T00:10:00Z".into(), duration_ms: 600_000 }
}

/// A Dreamcast game under Flycast in RetroArch, with its (shared) VMU in the saves folder.
struct Dc {
    t: tempfile::TempDir,
    rom: LocalRom,
    env: EmuEnv,
    st: State,
}

impl Dc {
    fn new(vmu_now: &[u8]) -> Dc {
        let t = tempfile::tempdir().unwrap();
        let env = retroarch_env(&t.path().join("ra"), "flycast");
        write(&t.path().join("ra"), "saves/vmu_save_A1.bin", vmu_now);
        let rom = rom("Some Game", "dc");
        let mut st = State::default();
        with_identity(&mut st, &rom, Identity { platform: "dreamcast".into(), ..Default::default() });
        Dc { t, rom, env, st }
    }
    fn play(&self, vmu: &[u8]) {
        write(&self.t.path().join("ra"), "saves/vmu_save_A1.bin", vmu);
    }
    fn key(&self) -> String {
        Route { layout: "flycast".into(), platform: "dreamcast".into(), root: self.t.path().join("ra/saves"), options: vec![] }.state_key()
    }
}

#[tokio::test]
async fn a_dreamcast_save_that_appears_during_a_session_is_claimed_for_that_game_and_uploaded() {
    let mut dc = Dc::new(&vmu_before_play());
    let server = MockServer::start().await;
    mock_negotiate_once(&server, vec![]).await; // before the game
    mock_negotiate_once(&server, vec![op("upload", None, "Some Game.bin")]).await; // after it
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(201).set_body_json(stored_json(41, "Some Game.bin", VMU_CLAIMED_HASH))).mount(&server).await;
    mock_complete(&server).await;
    let c = client(&server);
    let bak = dc.t.path().join("bak");

    // Before the game: nothing of this game's yet; the volume's other saves are only noted.
    let rep = syncer(&c, &mut dc.st, &bak, ConflictPolicy::KeepBoth).sync_rom(&dc.rom, &dc.env, None).await.unwrap();
    assert!(rep.is_clean() && rep.uploaded.is_empty(), "{rep:?}");
    assert_eq!(dc.st.sigil_unowned[&dc.key()], ["OTHER_SAV"]);

    // The game is played: it writes its save to the shared VMU. After the session, that new save, and
    // only that one, is the game's.
    dc.play(&vmu_with_both());
    let rep = syncer(&c, &mut dc.st, &bak, ConflictPolicy::KeepBoth).sync_rom(&dc.rom, &dc.env, Some(play_session())).await.unwrap();
    assert_eq!(rep.uploaded, ["Some Game.bin"], "{rep:?}");

    let up = &reqs(&server, "POST", "/api/saves").await[0];
    assert_eq!(query(up)["emulator"], "flycast");
    assert_eq!(query(up)["content_hash"], VMU_CLAIMED_HASH);
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[1].body_json().unwrap();
    assert_eq!(neg["saves"][0]["content_hash"], VMU_CLAIMED_HASH);

    assert_eq!(dc.st.sigil_unowned[&dc.key()], ["OTHER_SAV"], "the other game's save was not taken");
    let rec = &dc.st.saves[&save_key(7, "flycast")];
    assert_eq!(rec.last_hash.as_deref(), Some(VMU_CLAIMED_HASH));
    assert!(!rec.dirty, "transferred, so no longer pending");
}

#[tokio::test]
async fn nothing_is_claimed_the_first_time_a_volume_is_seen_or_outside_a_play_session() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let c = client(&server);

    // First time this volume is seen, even right after a session: there is no "since", so the saves
    // already on it must not be handed to whichever game happens to be running.
    let mut dc = Dc::new(&vmu_with_both());
    let bak = dc.t.path().join("bak");
    syncer(&c, &mut dc.st, &bak, ConflictPolicy::KeepBoth).sync_rom(&dc.rom, &dc.env, Some(play_session())).await.unwrap();
    assert_eq!(dc.st.sigil_unowned[&dc.key()], ["MYGAME_SAV", "OTHER_SAV"]);
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"], json!([]));

    // A plain sync (no session) after a new save appears doesn't claim it either.
    let mut dc = Dc::new(&vmu_before_play());
    let bak = dc.t.path().join("bak");
    syncer(&c, &mut dc.st, &bak, ConflictPolicy::KeepBoth).sync_rom(&dc.rom, &dc.env, None).await.unwrap();
    dc.play(&vmu_with_both());
    syncer(&c, &mut dc.st, &bak, ConflictPolicy::KeepBoth).sync_rom(&dc.rom, &dc.env, None).await.unwrap();
    let all = reqs(&server, "POST", "/api/sync/negotiate").await;
    assert_eq!(all.last().unwrap().body_json::<Value>().unwrap()["saves"], json!([]));
}

#[tokio::test]
async fn a_claimed_save_whose_upload_fails_is_still_sent_next_time() {
    // A claim is recorded only inside Sigil's state, which moves on at once. If that alone decided
    // "unchanged", a failed upload would leave the server's older copy looking current forever.
    let mut dc = Dc::new(&vmu_before_play());
    dc.st.saves.insert(save_key(7, "flycast"), romm_core::state::SaveRecord { last_hash: Some("OLDHASH".into()), server_save_id: Some(5), ..Default::default() });
    let server = MockServer::start().await;
    mock_negotiate_once(&server, vec![]).await;
    mock_negotiate_once(&server, vec![op("upload", None, "Some Game.bin")]).await;
    mock_negotiate(&server, vec![op("upload", None, "Some Game.bin")]).await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(500)).up_to_n_times(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(201).set_body_json(stored_json(41, "Some Game.bin", VMU_CLAIMED_HASH))).mount(&server).await;
    mock_complete(&server).await;
    let c = client(&server);
    let bak = dc.t.path().join("bak");

    syncer(&c, &mut dc.st, &bak, ConflictPolicy::KeepBoth).sync_rom(&dc.rom, &dc.env, None).await.unwrap();
    dc.play(&vmu_with_both());
    let rep = syncer(&c, &mut dc.st, &bak, ConflictPolicy::KeepBoth).sync_rom(&dc.rom, &dc.env, Some(play_session())).await.unwrap();
    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    assert!(dc.st.saves[&save_key(7, "flycast")].dirty);

    let rep = syncer(&c, &mut dc.st, &bak, ConflictPolicy::KeepBoth).sync_rom(&dc.rom, &dc.env, None).await.unwrap();
    assert_eq!(rep.uploaded, ["Some Game.bin"], "{rep:?}");
    let third: Value = reqs(&server, "POST", "/api/sync/negotiate").await[2].body_json().unwrap();
    assert_eq!(third["saves"][0]["content_hash"], VMU_CLAIMED_HASH, "not the stale OLDHASH");
    assert!(!dc.st.saves[&save_key(7, "flycast")].dirty);
}

// ============================================================================================
// PSP: the format this project got wrong
// ============================================================================================

#[tokio::test]
async fn psp_play_session_uploads_exactly_the_unit_argosy_uploads() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("src");
    psp_memory_stick(&src);
    let root = t.path().join("ppsspp/PSP");
    // A real disc image, named without its id: the game is identified from inside the image.
    let r = rom_disc(&t.path().join("roms"), "Crisis Core.iso", "psp", &psp_disc("ULUS-10064"));
    let name = "Crisis Core.zip";

    let server = MockServer::start().await;
    mock_negotiate_once(&server, vec![]).await; // before the game
    mock_negotiate_once(&server, vec![op("upload", None, name)]).await; // after it
    mock_negotiate(&server, vec![op("no_op", Some(21), name)]).await; // a later sync
    Mock::given(method("POST"))
        .and(path("/api/saves"))
        .respond_with(ResponseTemplate::new(201).set_body_json(stored_json(21, name, PSP_HASH)))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    // The "emulator" lays down the memory stick the way a real session would.
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(r#"mkdir -p "$0" && cp -r "$1/PSP/." "$0/""#).arg(&root).arg(&src);

    let env = standalone_env("ppsspp", &root);
    let c = client(&server);
    let mut st = State::default();
    let state_path = t.path().join("state.json");
    let out = romm_core::session::play(&c, DEVICE, &mut st, &state_path, t.path().join("bak"), &r, &env, cmd, ConflictPolicy::KeepBoth, None)
        .await
        .unwrap();
    assert!(out.pull_error.is_none() && out.push_error.is_none(), "{out:?}");

    // What went up: every folder of the game, each under its own name. No `SAVEDATA/` prefix, no
    // manifest, nothing of another game, and none of the installed game data.
    let up = &reqs(&server, "POST", "/api/saves").await[0];
    let body = body_text(up);
    for needle in ["ULUS10064DATA00/DATA.BIN", "ULUS10064DATA00/PARAM.SFO", "ULUS10064SETTINGS/OPT.BIN", &format!("filename=\"{name}\"")] {
        assert!(body.contains(needle), "missing {needle}");
    }
    // ("SAVEDATA/" with the slash: PARAM.SFO's own bytes contain the key SAVEDATA_PARAMS.)
    for needle in ["SAVEDATA/", "romm-bundle.json", "ULES00001", "ULUS10064/DATA.BIN"] {
        assert!(!body.contains(needle), "must not contain {needle}");
    }
    let q = query(up);
    assert_eq!(q["emulator"], "ppsspp");
    assert_eq!(q["slot"], "autosave");
    assert_eq!(q["content_hash"], PSP_HASH, "the hash RomM and Argosy compute for this save");

    // What was remembered: the exchange, and Sigil's state (committed only now the upload is done).
    let rec = &st.saves[&save_key(7, "ppsspp")];
    assert_eq!(rec.last_hash.as_deref(), Some(PSP_HASH));
    assert_eq!(rec.server_save_id, Some(21));
    assert_eq!(rec.server_updated_at.as_deref(), Some(SERVER_TIME));
    assert_eq!(st.sigil.len(), 1);

    // A later sync with nothing changed hands the server back its own hash and time.
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&r, &env, None).await.unwrap();
    assert_eq!(rep.unchanged, 1);
    let third: Value = reqs(&server, "POST", "/api/sync/negotiate").await[2].body_json().unwrap();
    assert_eq!(third["saves"][0]["content_hash"], PSP_HASH);
    assert_eq!(third["saves"][0]["updated_at"], SERVER_TIME);
    assert_eq!(third["saves"][0]["file_name"], name);
    assert_eq!(reqs(&server, "POST", "/api/saves").await.len(), 1);
}

#[tokio::test]
async fn psp_save_made_by_argosy_is_restored_and_the_old_local_one_is_kept() {
    let t = tempfile::tempdir().unwrap();
    let r = rom_file(&t.path().join("roms"), "Crisis Core [ULUS10064].iso", "psp");
    // The server's save, exactly as Argosy produced it.
    let argosy_unit = {
        let src = t.path().join("src");
        psp_memory_stick(&src);
        unit::collect(&psp_route(&src.join("PSP")), &unit::identify(&r.launch_path, "psp"), "x.iso", None).unwrap().unwrap()
    };
    assert_eq!(argosy_unit.content_hash, PSP_HASH);

    let root = t.path().join("ppsspp/PSP");
    // This machine already has an older save of its own and the game's installed data.
    write(&root, "SAVEDATA/ULUS10064DATA00/PARAM.SFO", &psp_save_sfo());
    write(&root, "SAVEDATA/ULUS10064DATA00/DATA.BIN", b"OLD-LOCAL");
    write(&root, "SAVEDATA/ULUS10064/DATA.BIN", b"installed");
    write(&root, "SAVEDATA/ULUS10064/PARAM.SFO", &psp_install_sfo());

    let server = MockServer::start().await;
    mock_negotiate_once(&server, vec![op("download", Some(21), "Crisis Core [ULUS10064].zip")]).await;
    mock_negotiate(&server, vec![op("no_op", Some(21), "Crisis Core [ULUS10064].zip")]).await;
    Mock::given(method("GET")).and(path("/api/saves/21/content")).respond_with(ResponseTemplate::new(200).set_body_bytes(argosy_unit.data.clone())).mount(&server).await;
    mock_complete(&server).await;

    let env = standalone_env("ppsspp", &root);
    let c = client(&server);
    let mut st = State::default();
    let bak = t.path().join("bak");
    let rep = syncer(&c, &mut st, &bak, ConflictPolicy::ServerWins).sync_rom(&r, &env, None).await.unwrap();
    assert!(rep.is_clean(), "{rep:?}");
    assert_eq!(rep.downloaded, ["Crisis Core [ULUS10064].zip"]);

    let sd = root.join("SAVEDATA");
    assert_eq!(std::fs::read(sd.join("ULUS10064DATA00/DATA.BIN")).unwrap(), vec![b'A'; 300]);
    assert_eq!(std::fs::read(sd.join("ULUS10064SETTINGS/OPT.BIN")).unwrap(), vec![b'B'; 120]);
    assert_eq!(std::fs::read(sd.join("ULUS10064/DATA.BIN")).unwrap(), b"installed", "game data is not a save");
    // The save that was replaced is kept, in the form it would have been uploaded.
    assert!(walk(&bak).iter().any(|p| p.extension().is_some_and(|e| e == "zip")), "{:?}", walk(&bak));

    // Baseline is the server's own hash, so the next sync sees nothing to do.
    let rec = &st.saves[&save_key(7, "ppsspp")];
    assert_eq!(rec.last_hash.as_deref(), Some(PSP_HASH));
    let rep = syncer(&c, &mut st, &bak, ConflictPolicy::ServerWins).sync_rom(&r, &env, None).await.unwrap();
    assert_eq!(rep.unchanged, 1);
    let second: Value = reqs(&server, "POST", "/api/sync/negotiate").await[1].body_json().unwrap();
    assert_eq!(second["saves"][0]["content_hash"], PSP_HASH);
}

/// A zip as older romm-desktop versions uploaded: a manifest, and paths relative to the data folder.
fn legacy_bundle(files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    use zip::{write::SimpleFileOptions, ZipWriter};
    let o = SimpleFileOptions::default();
    let mut z = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let names: Vec<&str> = files.iter().map(|(n, _)| *n).collect();
    z.start_file("romm-bundle.json", o).unwrap();
    z.write_all(json!({"format": 1, "emulator": "ppsspp", "files": names}).to_string().as_bytes()).unwrap();
    for (n, d) in files {
        z.start_file(*n, o).unwrap();
        z.write_all(d).unwrap();
    }
    z.finish().unwrap().into_inner()
}

#[tokio::test]
async fn a_save_uploaded_by_an_older_romm_desktop_is_restored_then_replaced_with_argosys_form() {
    let t = tempfile::tempdir().unwrap();
    let r = rom_file(&t.path().join("roms"), "Crisis Core [ULUS10064].iso", "psp");
    let legacy = legacy_bundle(&[("SAVEDATA/ULUS10064DATA00/PARAM.SFO", &psp_save_sfo()), ("SAVEDATA/ULUS10064DATA00/DATA.BIN", &[b'L'; 40])]);

    // What the converted save must hash to: the same two files, collected the Argosy way.
    let expected = {
        let src = t.path().join("exp");
        write(&src, "PSP/SAVEDATA/ULUS10064DATA00/PARAM.SFO", &psp_save_sfo());
        write(&src, "PSP/SAVEDATA/ULUS10064DATA00/DATA.BIN", &[b'L'; 40]);
        unit::collect(&psp_route(&src.join("PSP")), &unit::identify(&r.launch_path, "psp"), "x.iso", None).unwrap().unwrap().content_hash
    };

    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("download", Some(3), "Crisis Core [ULUS10064].zip")]).await;
    Mock::given(method("GET")).and(path("/api/saves/3/content")).respond_with(ResponseTemplate::new(200).set_body_bytes(legacy)).mount(&server).await;
    Mock::given(method("PUT"))
        .and(path("/api/saves/3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(stored_json(3, "Crisis Core [ULUS10064].zip", &expected)))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let root = t.path().join("ppsspp/PSP");
    let env = standalone_env("ppsspp", &root);
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::ServerWins).sync_rom(&r, &env, None).await.unwrap();
    assert!(rep.is_clean(), "{rep:?}");

    // Put back where the old version wrote it...
    assert_eq!(std::fs::read(root.join("SAVEDATA/ULUS10064DATA00/DATA.BIN")).unwrap(), vec![b'L'; 40]);
    // ...and the server's copy was replaced by the same save in Argosy's form.
    let put = &reqs(&server, "PUT", "/api/saves/3").await[0];
    let body = body_text(put);
    assert!(body.contains("ULUS10064DATA00/DATA.BIN"), "{body}");
    assert!(!body.contains("romm-bundle.json") && !body.contains("SAVEDATA/"), "{body}");
    assert_eq!(query(put)["content_hash"], expected);
    assert_eq!(query(put)["emulator"], "ppsspp");
    assert_eq!(st.saves[&save_key(7, "ppsspp")].last_hash.as_deref(), Some(expected.as_str()));
    assert_eq!(st.sigil.len(), 1);
}

#[tokio::test]
async fn a_failed_upload_leaves_the_save_marked_changed() {
    // Sigil's state must not advance until the save has reached RomM, or a failed upload would
    // lose the marker and the change would never be sent.
    let t = tempfile::tempdir().unwrap();
    let r = rom_file(&t.path().join("roms"), "Crisis Core [ULUS10064].iso", "psp");
    let root = t.path().join("ppsspp/PSP");
    psp_memory_stick(&t.path().join("src"));
    std::fs::create_dir_all(&root).unwrap();
    copy_dir(&t.path().join("src/PSP"), &root);
    let name = "Crisis Core [ULUS10064].zip";

    let server = MockServer::start().await;
    mock_negotiate_once(&server, vec![op("upload", None, name)]).await;
    mock_negotiate(&server, vec![op("upload", None, name)]).await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(500)).up_to_n_times(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(201).set_body_json(stored_json(21, name, PSP_HASH))).mount(&server).await;
    mock_complete(&server).await;

    let env = standalone_env("ppsspp", &root);
    let c = client(&server);
    let mut st = State::default();
    let bak = t.path().join("bak");
    let rep = syncer(&c, &mut st, &bak, ConflictPolicy::KeepBoth).sync_rom(&r, &env, None).await.unwrap();
    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    assert!(st.sigil.is_empty(), "state must not advance past a failed upload");
    assert!(!st.saves.contains_key(&save_key(7, "ppsspp")));

    // Next time the save is still new, and goes up.
    let rep = syncer(&c, &mut st, &bak, ConflictPolicy::KeepBoth).sync_rom(&r, &env, None).await.unwrap();
    assert_eq!(rep.uploaded, [name]);
    assert_eq!(st.saves[&save_key(7, "ppsspp")].last_hash.as_deref(), Some(PSP_HASH));
    assert_eq!(st.sigil.len(), 1);
}

#[tokio::test]
async fn when_the_server_already_has_the_save_it_becomes_the_baseline() {
    // First sync on a machine whose save is identical to the server's: nothing to transfer, but it
    // must be remembered, or the next change would be measured from nothing.
    let t = tempfile::tempdir().unwrap();
    let r = rom_file(&t.path().join("roms"), "Crisis Core [ULUS10064].iso", "psp");
    let root = t.path().join("ppsspp/PSP");
    psp_memory_stick(&t.path().join("src"));
    copy_dir(&t.path().join("src/PSP"), &root);
    let name = "Crisis Core [ULUS10064].zip";

    let server = MockServer::start().await;
    mock_negotiate(&server, vec![json!({"action": "no_op", "rom_id": 7, "save_id": 21, "file_name": name, "slot": "autosave", "emulator": "ppsspp_gold", "server_content_hash": PSP_HASH, "server_updated_at": SERVER_TIME})]).await;
    mock_complete(&server).await;

    let env = standalone_env("ppsspp", &root);
    let c = client(&server);
    let mut st = State::default();
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&r, &env, None).await.unwrap();
    assert_eq!(rep.unchanged, 1);
    let rec = &st.saves[&save_key(7, "ppsspp")];
    assert_eq!(rec.last_hash.as_deref(), Some(PSP_HASH));
    assert_eq!(rec.server_save_id, Some(21));
    assert_eq!(rec.server_updated_at.as_deref(), Some(SERVER_TIME));
    assert_eq!(st.sigil.len(), 1);
}

fn copy_dir(from: &Path, to: &Path) {
    for p in walk(from) {
        let rel = p.strip_prefix(from).unwrap();
        write(to, &rel.to_string_lossy(), &std::fs::read(&p).unwrap());
    }
}

#[tokio::test]
async fn offline_play_still_runs_and_the_save_is_sent_by_the_next_sync() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("src");
    psp_memory_stick(&src);
    let root = t.path().join("ppsspp/PSP");
    let r = rom_file(&t.path().join("roms"), "Crisis Core [ULUS10064].iso", "psp");
    let name = "Crisis Core [ULUS10064].zip";

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(r#"mkdir -p "$0" && cp -r "$1/PSP/." "$0/""#).arg(&root).arg(&src);
    let env = standalone_env("ppsspp", &root);
    let dead = RommClient::new("http://127.0.0.1:9", Some("rmm_test".into()), false).unwrap();
    let mut st = State::default();
    let state_path = t.path().join("state.json");
    let out = romm_core::session::play(&dead, DEVICE, &mut st, &state_path, t.path().join("bak"), &r, &env, cmd, ConflictPolicy::KeepBoth, None)
        .await
        .unwrap();
    assert!(out.pull_error.is_some() && out.push_error.is_some(), "{out:?}");
    // Nothing reached the server, so nothing may be marked as synced.
    assert!(st.sigil.is_empty() && st.saves.is_empty());
    assert!(root.join("SAVEDATA/ULUS10064DATA00/DATA.BIN").exists(), "the game still ran");

    // Back online: the save is still new, and goes up.
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("upload", None, name)]).await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(201).set_body_json(stored_json(21, name, PSP_HASH))).mount(&server).await;
    mock_complete(&server).await;
    let c = client(&server);
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&r, &env, None).await.unwrap();
    assert_eq!(rep.uploaded, [name]);
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"][0]["content_hash"], PSP_HASH);
}

#[tokio::test]
async fn a_psp_game_that_cannot_be_identified_says_so_and_still_records_the_session() {
    // No id in the ROM (it's empty) or its name: there is no telling which SAVEDATA folders are
    // this game's. That must be said plainly, not guessed at, and must not cost the playtime.
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("ppsspp/PSP");
    psp_memory_stick(&t.path().join("src"));
    copy_dir(&t.path().join("src/PSP"), &root);
    let r = rom_file(&t.path().join("roms"), "Daxter.iso", "psp");

    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let env = standalone_env("ppsspp", &root);
    let c = client(&server);
    let mut st = State::default();
    let play = romm_core::api::types::PlaySession { rom_id: 7, save_slot: None, start_time: "2026-01-01T00:00:00Z".into(), end_time: "2026-01-01T00:10:00Z".into(), duration_ms: 600_000 };
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&r, &env, Some(play)).await.unwrap();

    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    assert!(rep.failed[0].contains("can't tell which game") && rep.failed[0].contains("[ULUS10064]"), "{}", rep.failed[0]);
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"], json!([]));
    assert_eq!(neg["rom_ids"], json!([]), "nothing is claimed about a game that can't be identified");
    let done: Value = reqs(&server, "POST", "/api/sync/sessions/42/complete").await[0].body_json().unwrap();
    assert_eq!(done["play_sessions"].as_array().map(Vec::len), Some(1), "playtime is still recorded");
}

// ============================================================================================
// Other standalone emulators
// ============================================================================================

#[tokio::test]
async fn gamecube_upload_carries_only_this_games_gci_files_under_the_dolphin_label() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("dolphin");
    dolphin_gc_folder(&root);
    let r = rom("Melee", "ngc");
    let mut st = State::default();
    with_identity(&mut st, &r, Identity { platform: "gamecube".into(), title_id: "47414C45".into(), save_id: "GALE".into(), ..Default::default() });

    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("upload", None, "Melee.zip")]).await;
    Mock::given(method("POST")).and(path("/api/saves")).respond_with(ResponseTemplate::new(201).set_body_json(stored_json(31, "Melee.zip", GC_HASH))).mount(&server).await;
    mock_complete(&server).await;

    let env = standalone_env("dolphin", &root);
    let c = client(&server);
    let rep = syncer(&c, &mut st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&r, &env, None).await.unwrap();
    assert_eq!(rep.uploaded, ["Melee.zip"], "{rep:?}");

    let up = &reqs(&server, "POST", "/api/saves").await[0];
    let body = body_text(up);
    assert!(body.contains("01-GALE-melee_save.gci") && body.contains("01-GALE-melee_opts.gci"), "{body}");
    assert!(!body.contains("zelda") && !body.contains("GZLE"), "another game's save must stay out");
    assert_eq!(query(up)["emulator"], "dolphin");
    assert_eq!(query(up)["content_hash"], GC_HASH);
}

#[tokio::test]
async fn emulator_that_fails_to_start_is_an_error() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let t = tempfile::tempdir().unwrap();
    let env = standalone_env("pcsx2", &t.path().join("pcsx2"));
    let c = client(&server);
    let mut st = State::default();
    let cmd = tokio::process::Command::new(t.path().join("does-not-exist"));
    let err = romm_core::session::play(&c, DEVICE, &mut st, &t.path().join("s.json"), t.path().join("bak"), &rom("Kingdom Hearts", "ps2"), &env, cmd, ConflictPolicy::KeepBoth, None)
        .await
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("launch") || err.to_string().to_lowercase().contains("start") || !err.to_string().is_empty());
}

// ============================================================================================
// xemu: saves are folders inside a virtual hard disk, read out of it and uploaded (never restored)
// ============================================================================================

fn xbox_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xbox").join(name)
}

/// RomM's `content_hash` of one title's save, worked out from the fixture manifest alone (libfatx
/// re-read the volume to write it), so it checks the bytes uploaded rather than repeating our code.
fn xbox_expected_hash(id: &str) -> String {
    let m: Value = serde_json::from_slice(&std::fs::read(xbox_fixture("manifest.json")).unwrap()).unwrap();
    let mut lines: Vec<String> = m["e"]["titles"][id]["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(p, f)| format!("{id}/{p}:{}", f["md5"].as_str().unwrap()))
        .collect();
    lines.sort();
    saves::md5_bytes(lines.join("\n").as_bytes())
}

struct Xemu {
    env: EmuEnv,
    rom: LocalRom,
    st: State,
    image: PathBuf,
}

/// An xemu data folder holding a copy of the reference disk image, and an Xbox game whose XBE
/// carries `save_id` (as if it had been synced before, so no ISO is needed).
fn xemu_setup(dir: &Path, save_id: &str) -> Xemu {
    let root = dir.join("xemu");
    std::fs::create_dir_all(&root).unwrap();
    let image = root.join("xbox_hdd.qcow2");
    std::fs::copy(xbox_fixture("hdd-v3.qcow2"), &image).unwrap();
    let rom = rom("Halo", "xbox");
    let mut st = State::default();
    with_identity(&mut st, &rom, Identity { platform: "xbox".into(), save_id: save_id.into(), ..Default::default() });
    Xemu { env: standalone_env("xemu", &root), rom, st, image }
}

async fn mock_save_upload(server: &MockServer, hash: &str) {
    Mock::given(method("POST"))
        .and(path("/api/saves"))
        .respond_with(ResponseTemplate::new(201).set_body_json(stored_json(11, "Halo.zip", hash)))
        .mount(server)
        .await;
}

#[tokio::test]
async fn xemu_uploads_the_games_udata_folder_from_the_disk_image() {
    let server = MockServer::start().await;
    let want = xbox_expected_hash("4D530004");
    mock_negotiate(&server, vec![op("upload", None, "Halo.zip")]).await;
    mock_save_upload(&server, &want).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let mut x = xemu_setup(t.path(), "4D530004");
    let before = std::fs::read(&x.image).unwrap();
    let c = client(&server);
    let rep = syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&x.rom, &x.env, None).await.unwrap();
    assert!(rep.is_clean(), "{rep:?}");
    assert_eq!(rep.uploaded, ["Halo.zip"]);

    // What was announced and what was sent: one save, labelled xemu, the zip libfatx says it is.
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"][0]["emulator"], "xemu");
    assert_eq!(neg["saves"][0]["slot"], "autosave");
    assert_eq!(neg["saves"][0]["file_name"], "Halo.zip");
    assert_eq!(neg["saves"][0]["content_hash"], want.as_str());
    let up = &reqs(&server, "POST", "/api/saves").await[0];
    assert_eq!(query(up)["emulator"], "xemu");
    assert_eq!(query(up)["content_hash"], want);
    assert!(body_text(up).contains("4D530004/"), "the zip is rooted at the upper-case title id");

    // Remembered, and the disk was only read.
    assert_eq!(x.st.saves[&save_key(7, "xemu")].last_hash.as_deref(), Some(want.as_str()));
    assert_eq!(std::fs::read(&x.image).unwrap(), before, "the disk image must not be touched");
}

#[tokio::test]
async fn xemu_save_that_has_not_changed_is_not_uploaded_again() {
    let server = MockServer::start().await;
    let want = xbox_expected_hash("4D530004");
    mock_negotiate_once(&server, vec![op("upload", None, "Halo.zip")]).await;
    mock_negotiate(&server, vec![op("no_op", Some(11), "Halo.zip")]).await;
    mock_save_upload(&server, &want).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let mut x = xemu_setup(t.path(), "4D530004");
    let c = client(&server);
    syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&x.rom, &x.env, None).await.unwrap();
    assert_eq!(saves_overview(&x.env, &x.rom, &x.st).unwrap()[0].status, SaveStatus::Synced);

    let rep = syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&x.rom, &x.env, None).await.unwrap();
    assert!(rep.is_clean() && rep.uploaded.is_empty() && rep.unchanged == 1, "{rep:?}");
    assert_eq!(reqs(&server, "POST", "/api/saves").await.len(), 1);
}

#[tokio::test]
async fn xemu_never_writes_a_server_save_into_the_disk_image() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![op("download", Some(99), "Halo.zip")]).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    // A title whose folder has no files: this machine has nothing, the server has a save.
    let mut x = xemu_setup(t.path(), "55550001");
    let before = std::fs::read(&x.image).unwrap();
    let c = client(&server);
    let rep = syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&x.rom, &x.env, None).await.unwrap();

    assert!(rep.downloaded.is_empty(), "{rep:?}");
    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    assert!(rep.failed[0].contains("isn't supported yet"), "{rep:?}");
    assert!(reqs(&server, "GET", "/api/saves/99/content").await.is_empty(), "nothing is downloaded for a save that can't be applied");
    assert_eq!(std::fs::read(&x.image).unwrap(), before);
}

#[tokio::test]
async fn xemu_conflict_is_never_settled_by_taking_the_servers_save() {
    for policy in [ConflictPolicy::KeepBoth, ConflictPolicy::ServerWins, ConflictPolicy::Ask] {
        let server = MockServer::start().await;
        mock_negotiate(&server, vec![op("conflict", Some(5), "Halo.zip")]).await;
        mock_complete(&server).await;
        let t = tempfile::tempdir().unwrap();
        let mut x = xemu_setup(t.path(), "4D530004");
        let before = std::fs::read(&x.image).unwrap();
        let c = client(&server);
        let rep = syncer(&c, &mut x.st, &t.path().join("bak"), policy).sync_rom(&x.rom, &x.env, None).await.unwrap();

        assert_eq!(rep.conflicts.len(), 1, "{policy:?}: {rep:?}");
        assert!(rep.failed.is_empty() && rep.downloaded.is_empty() && rep.archived.is_empty(), "{policy:?}: {rep:?}");
        // Keeping both would archive a copy on the server at every sync; none of these may upload.
        assert!(reqs(&server, "POST", "/api/saves").await.is_empty(), "{policy:?}");
        assert!(reqs(&server, "PUT", "/api/saves/5").await.is_empty(), "{policy:?}");
        assert!(reqs(&server, "GET", "/api/saves/5/content").await.is_empty(), "{policy:?}");
        assert_eq!(std::fs::read(&x.image).unwrap(), before, "{policy:?}");
    }
}

#[tokio::test]
async fn xemu_conflict_can_still_be_settled_in_favour_of_the_disk() {
    let server = MockServer::start().await;
    let want = xbox_expected_hash("4D530004");
    mock_negotiate(&server, vec![op("conflict", Some(5), "Halo.zip")]).await;
    Mock::given(method("PUT"))
        .and(path("/api/saves/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(stored_json(5, "Halo.zip", &want)))
        .mount(&server)
        .await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let mut x = xemu_setup(t.path(), "4D530004");
    let c = client(&server);
    let rep = syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::DeviceWins).sync_rom(&x.rom, &x.env, None).await.unwrap();
    assert_eq!(rep.uploaded, ["Halo.zip"], "{rep:?}");
    assert_eq!(query(&reqs(&server, "PUT", "/api/saves/5").await[0])["content_hash"], want);
}

#[tokio::test]
async fn xemu_game_with_no_save_folder_has_nothing_to_upload() {
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let t = tempfile::tempdir().unwrap();
    for id in ["FFFFFFFF", "55550001"] {
        let mut x = xemu_setup(t.path(), id);
        let c = client(&server);
        let rep = syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&x.rom, &x.env, None).await.unwrap();
        assert!(rep.is_clean(), "{id}: {rep:?}");
        assert!(saves_overview(&x.env, &x.rom, &x.st).unwrap().is_empty(), "{id}");
    }
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!(neg["saves"], json!([]));
}

#[tokio::test]
async fn xemu_follows_the_disk_image_path_in_xemu_toml() {
    let server = MockServer::start().await;
    let want = xbox_expected_hash("4D530004");
    mock_negotiate(&server, vec![op("upload", None, "Halo.zip")]).await;
    mock_save_upload(&server, &want).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    let mut x = xemu_setup(t.path(), "4D530004");
    // The image lives elsewhere, as xemu's own settings say; the default name no longer exists.
    let moved = t.path().join("disks/hdd.qcow2");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&x.image, &moved).unwrap();
    std::fs::write(x.env.data_root.join("xemu.toml"), format!("[sys.files]\nhdd_path = '{}'\n", moved.display())).unwrap();

    let c = client(&server);
    let rep = syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&x.rom, &x.env, None).await.unwrap();
    assert_eq!(rep.uploaded, ["Halo.zip"], "{rep:?}");
}

#[tokio::test]
async fn xemu_without_a_readable_disk_syncs_nothing_and_says_why() {
    let t = tempfile::tempdir().unwrap();
    let c_and = |server: &MockServer| client(server);

    // 1. No disk image where xemu says it is.
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let mut x = xemu_setup(t.path(), "4D530004");
    std::fs::remove_file(&x.image).unwrap();
    let c = c_and(&server);
    let rep = syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&x.rom, &x.env, None).await.unwrap();
    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    assert!(rep.failed[0].contains("hard disk image") && rep.failed[0].contains("xbox_hdd.qcow2"), "{rep:?}");
    let neg: Value = reqs(&server, "POST", "/api/sync/negotiate").await[0].body_json().unwrap();
    assert_eq!((neg["saves"].clone(), neg["rom_ids"].clone()), (json!([]), json!([])), "an unreadable save must not look like \"no save\"");

    // 2. A disk xemu has open (QEMU's dirty flag, bit 0 of the incompatible features): half-written
    //    data must not be uploaded over a good save.
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let mut x = xemu_setup(t.path(), "4D530004");
    let mut img = std::fs::read(&x.image).unwrap();
    assert_eq!(&img[4..8], &3u32.to_be_bytes(), "the fixture is a v3 image");
    img[79] |= 1;
    std::fs::write(&x.image, img).unwrap();
    let c = c_and(&server);
    let rep = syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&x.rom, &x.env, None).await.unwrap();
    assert!(rep.failed[0].contains("in use") && rep.failed[0].contains("Close xemu"), "{rep:?}");
    assert!(reqs(&server, "POST", "/api/saves").await.is_empty());

    // 3. A ROM whose title id could not be read.
    let server = MockServer::start().await;
    mock_negotiate(&server, vec![]).await;
    mock_complete(&server).await;
    let mut x = xemu_setup(t.path(), "");
    x.st = State::default();
    let c = c_and(&server);
    let rep = syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&x.rom, &x.env, None).await.unwrap();
    assert!(rep.failed[0].contains("title id") && rep.failed[0].contains("XISO"), "{rep:?}");
}

/// The smallest XISO Sigil accepts: a volume descriptor at the trimmed-image position, a root
/// directory holding only `default.xbe`, and an XBE whose certificate carries `title_id`.
fn minimal_xiso(title_id: u32) -> Vec<u8> {
    const SECTOR: usize = 2048;
    let (root_sector, xbe_sector) = (33usize, 34usize);
    let mut img = vec![0u8; xbe_sector * SECTOR + 0x400];

    let vd = 32 * SECTOR;
    img[vd..vd + 20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
    img[vd + 0x14..vd + 0x18].copy_from_slice(&(root_sector as u32).to_le_bytes());
    img[vd + 0x18..vd + 0x1C].copy_from_slice(&(SECTOR as u32).to_le_bytes());
    img[vd + 0x7EC..vd + 0x7EC + 20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");

    // One directory node: no left/right child, the file's sector and size, attribute 0x20, name.
    let dir = root_sector * SECTOR;
    img[dir + 4..dir + 8].copy_from_slice(&(xbe_sector as u32).to_le_bytes());
    img[dir + 8..dir + 12].copy_from_slice(&0x400u32.to_le_bytes());
    img[dir + 12] = 0x20;
    img[dir + 13] = "default.xbe".len() as u8;
    img[dir + 14..dir + 14 + 11].copy_from_slice(b"default.xbe");

    // XBE: image base 0x10000, certificate at virtual address 0x10200 (file offset 0x200).
    let xbe = xbe_sector * SECTOR;
    img[xbe..xbe + 4].copy_from_slice(b"XBEH");
    img[xbe + 0x104..xbe + 0x108].copy_from_slice(&0x10000u32.to_le_bytes());
    img[xbe + 0x118..xbe + 0x11C].copy_from_slice(&0x10200u32.to_le_bytes());
    img[xbe + 0x200..xbe + 0x204].copy_from_slice(&0x1D0u32.to_le_bytes());
    img[xbe + 0x208..xbe + 0x20C].copy_from_slice(&title_id.to_le_bytes());
    img
}

#[tokio::test]
async fn xemu_finds_the_title_id_by_reading_the_xbe_inside_the_xiso() {
    let server = MockServer::start().await;
    let want = xbox_expected_hash("4D530004");
    mock_negotiate(&server, vec![op("upload", None, "Halo.zip")]).await;
    mock_save_upload(&server, &want).await;
    mock_complete(&server).await;

    let t = tempfile::tempdir().unwrap();
    // No cached identity: Sigil has to read it from the disc image itself (0x4D530004 is "MS-004").
    let mut x = xemu_setup(t.path(), "");
    x.st = State::default();
    x.rom = rom_disc(&t.path().join("roms"), "Halo.iso", "xbox", &minimal_xiso(0x4D53_0004));
    x.st.roms.insert(x.rom.rom_id, x.rom.clone()); // an installed game, as the app has it

    let c = client(&server);
    let rep = syncer(&c, &mut x.st, &t.path().join("bak"), ConflictPolicy::KeepBoth).sync_rom(&x.rom, &x.env, None).await.unwrap();
    assert!(rep.is_clean(), "{rep:?}");
    assert_eq!(rep.uploaded, ["Halo.zip"]);
    assert_eq!(x.st.roms[&7].identity.as_ref().unwrap().save_id, "4D530004");
    assert_eq!(query(&reqs(&server, "POST", "/api/saves").await[0])["content_hash"], want);
}
