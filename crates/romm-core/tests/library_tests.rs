use std::io::Write;

use romm_core::api::types::{Platform, Rom};
use romm_core::api::RommClient;
use romm_core::library::{install_bios, install_rom, BiosStatus};
use romm_core::saves::md5_bytes;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(s: &MockServer) -> RommClient {
    RommClient::new(&s.uri(), Some("rmm_t".into()), false).unwrap()
}

#[tokio::test]
async fn single_file_rom_installs_into_platform_folder_and_is_not_redownloaded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/roms/7/content/Zelda.sfc"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ROMDATA".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let rom: Rom = serde_json::from_value(json!({
        "id": 7, "name": "Zelda", "fs_name": "Zelda.sfc", "fs_name_no_ext": "Zelda", "fs_size_bytes": 7,
        "platform_id": 3, "platform_slug": "snes", "platform_fs_slug": "snes"
    })).unwrap();
    let t = tempfile::tempdir().unwrap();
    let c = client(&server);
    let a = install_rom(&c, &rom, t.path(), &mut |_, _| {}).await.unwrap();
    assert_eq!(a.launch_path, t.path().join("snes/Zelda.sfc"));
    assert_eq!(a.stem, "Zelda");
    assert_eq!(std::fs::read(&a.launch_path).unwrap(), b"ROMDATA");
    // Second install finds the right size on disk and skips the network (expect(1) enforces it).
    install_rom(&c, &rom, t.path(), &mut |_, _| {}).await.unwrap();
}

#[tokio::test]
async fn zipped_single_file_rom_stays_zipped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/roms/8/content/pacman.zip"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PK\x03\x04arcade".to_vec()))
        .mount(&server)
        .await;
    let rom: Rom = serde_json::from_value(json!({
        "id": 8, "fs_name": "pacman.zip", "fs_name_no_ext": "pacman", "platform_id": 1,
        "platform_slug": "arcade", "platform_fs_slug": "arcade", "has_multiple_files": false
    })).unwrap();
    let t = tempfile::tempdir().unwrap();
    let l = install_rom(&client(&server), &rom, t.path(), &mut |_, _| {}).await.unwrap();
    assert!(l.launch_path.ends_with("arcade/pacman.zip") && l.launch_path.is_file());
}

#[tokio::test]
async fn multi_disc_rom_is_extracted_and_launches_the_playlist() {
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    for (n, d) in [("FF7.m3u", "d1.cue\nd2.cue\n"), ("d1.cue", "cue1"), ("d1.bin", "bin1"), ("d2.cue", "cue2"), ("d2.bin", "bin2")] {
        zw.start_file(n, o).unwrap();
        zw.write_all(d.as_bytes()).unwrap();
    }
    let zip_bytes = zw.finish().unwrap().into_inner();

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/roms/9/content/Final%20Fantasy%20VII"))
        .and(query_param("file_ids", "11,12"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(zip_bytes))
        .mount(&server)
        .await;
    let rom: Rom = serde_json::from_value(json!({
        "id": 9, "name": "Final Fantasy VII", "fs_name": "Final Fantasy VII", "fs_name_no_ext": "Final Fantasy VII",
        "platform_id": 5, "platform_slug": "psx", "platform_fs_slug": "psx", "has_multiple_files": true,
        "files": [{"id": 11, "rom_id": 9, "file_name": "d1.cue"}, {"id": 12, "rom_id": 9, "file_name": "d2.cue"}]
    })).unwrap();
    let t = tempfile::tempdir().unwrap();
    let l = install_rom(&client(&server), &rom, t.path(), &mut |_, _| {}).await.unwrap();
    assert_eq!(l.launch_path, t.path().join("psx/Final Fantasy VII/FF7.m3u"));
    assert_eq!(l.stem, "FF7");
    assert!(t.path().join("psx/Final Fantasy VII/d2.bin").is_file());
    assert!(!t.path().join("psx/.Final Fantasy VII.download.zip").exists(), "temp zip must be cleaned up");
}

fn psx_platform(md5: &str) -> Platform {
    serde_json::from_value(json!({
        "id": 5, "slug": "psx", "fs_slug": "psx", "name": "PlayStation",
        "firmware": [{"id": 9, "platform_id": 5, "file_name": "scph5501.bin", "md5_hash": md5}]
    })).unwrap()
}

#[tokio::test]
async fn bios_is_verified_and_skipped_when_already_correct() {
    let data = b"BIOSBYTES".to_vec();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/firmware/9/content/scph5501.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let t = tempfile::tempdir().unwrap();
    let p = psx_platform(&md5_bytes(&data));
    let c = client(&server);
    let r = install_bios(&c, &p, &t.path().join("bios"), &mut |_, _| {}).await.unwrap();
    assert_eq!(r[0].status, BiosStatus::Installed);
    let r = install_bios(&c, &p, &t.path().join("bios"), &mut |_, _| {}).await.unwrap();
    assert_eq!(r[0].status, BiosStatus::AlreadyPresent);
}

#[tokio::test]
async fn corrupt_bios_is_rejected_and_removed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/firmware/9/content/scph5501.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"CORRUPT".to_vec()))
        .mount(&server)
        .await;
    let t = tempfile::tempdir().unwrap();
    let p = psx_platform("490f666e1afb15b7362b406ed1cea246");
    let err = install_bios(&client(&server), &p, &t.path().join("bios"), &mut |_, _| {}).await.unwrap_err();
    assert!(err.to_string().contains("checksum mismatch"), "{err}");
    assert!(!t.path().join("bios/scph5501.bin").exists());
}
