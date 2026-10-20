//! Typed client for the RomM REST API.

pub mod types;

use std::path::Path;
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::{multipart, Response, Url};
use serde::de::DeserializeOwned;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use types::*;

pub const CLIENT_SLUG: &str = "romm-desktop";

/// Accepts `192.168.1.10:8080`, `http://host`, `https://host/` and so on.
pub fn normalize_base_url(input: &str) -> Result<String> {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() {
        return Err(Error::msg("server URL is empty"));
    }
    let with_scheme = if s.contains("://") { s.to_string() } else { format!("http://{s}") };
    Url::parse(&with_scheme).map_err(|e| Error::msg(format!("invalid server URL: {e}")))?;
    Ok(with_scheme)
}

#[derive(Clone)]
pub struct RommClient {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
    /// Shared by every clone: API calls take turns until their response headers arrive. See `send`.
    gate: Arc<Mutex<()>>,
}

impl RommClient {
    pub fn new(base_url: &str, token: Option<String>, accept_invalid_certs: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(format!("{CLIENT_SLUG}/{}", env!("CARGO_PKG_VERSION")))
            .danger_accept_invalid_certs(accept_invalid_certs)
            .build()?;
        Ok(Self { http, base: normalize_base_url(base_url)?, token, gate: Arc::new(Mutex::new(())) })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// `{base}/api/{segments...}` with every segment percent-encoded.
    fn url(&self, segments: &[&str]) -> Result<Url> {
        let mut url = Url::parse(&self.base).map_err(|e| Error::msg(e.to_string()))?;
        {
            let mut p = url
                .path_segments_mut()
                .map_err(|_| Error::msg("server URL cannot be a base"))?;
            p.pop_if_empty().push("api");
            for s in segments {
                p.push(s);
            }
        }
        Ok(url)
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    /// Sends one `/api` request and returns once the response *headers* are in.
    ///
    /// For every request made with a client token, RomM's auth middleware updates the token's,
    /// the device's and the user's "last seen" rows (the token and device ones at most every
    /// 5 minutes). When two requests arrive together while those are due, they race to update
    /// the same rows, and on MariaDB with snapshot isolation the loser gets an HTTP 500 (error
    /// 1020, "Record has changed since last read in table 'client_tokens'"). The app issues
    /// several calls at once (library page + platforms, background sync, details panel), so
    /// they take turns here: the auth step of one finishes before the next one starts.
    /// Bodies and downloads stream after the lock is released, so only the wait for headers is
    /// serialised. Not used for `/assets` (served by nginx, never reaches RomM's auth).
    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<Response> {
        let _turn = self.gate.lock().await;
        Ok(rb.send().await?)
    }

    async fn check(resp: Response) -> Result<Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let url = resp.url().to_string();
        let mut body = resp.text().await.unwrap_or_default();
        body.truncate(500);
        Err(Error::Status { status, url, body })
    }

    async fn get_json<T: DeserializeOwned>(&self, segments: &[&str], query: &[(&str, String)]) -> Result<T> {
        let resp = self.send(self.authed(self.http.get(self.url(segments)?).query(query))).await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    // ---------------------------------------------------------------- auth

    /// Exchange the 8-digit code shown in RomM's web UI (Profile → Client API
    /// Tokens → Pair) for a long-lived `rmm_…` token. Codes are single-use and
    /// expire after 5 minutes.
    pub async fn exchange_pair_code(base_url: &str, code: &str, accept_invalid_certs: bool) -> Result<TokenExchange> {
        let client = Self::new(base_url, None, accept_invalid_certs)?;
        let resp = client
            .http
            .post(client.url(&["client-tokens", "exchange"])?)
            .json(&serde_json::json!({ "code": code.trim() }))
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn heartbeat(&self) -> Result<serde_json::Value> {
        self.get_json(&["heartbeat"], &[]).await
    }

    // ------------------------------------------------------------- library

    pub async fn platforms(&self) -> Result<Vec<Platform>> {
        self.get_json(&["platforms"], &[]).await
    }

    pub async fn roms(
        &self,
        platform_id: Option<i64>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Page<Rom>> {
        let mut q: Vec<(&str, String)> = vec![
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
            ("order_by", "name".into()),
            ("with_files", "true".into()),
            // We don't need the filter/char indexes; skipping them keeps big libraries fast.
            ("with_char_index", "false".into()),
            ("with_filter_values", "false".into()),
            ("with_rom_id_index", "false".into()),
        ];
        if let Some(id) = platform_id {
            q.push(("platform_ids", id.to_string()));
        }
        if let Some(s) = search.filter(|s| !s.is_empty()) {
            q.push(("search_term", s.to_string()));
        }
        self.get_json(&["roms"], &q).await
    }

    /// The whole library, one entry per *game*: RomM's `group_by_meta_id` collapses the regional
    /// and revision copies of a title into the one it considers primary (the account's
    /// "default" copy, else the best by the server's region priority; see `versions`), and
    /// lists the rest in that entry's `sibling_roms`.
    ///
    /// Everything is fetched, page by page, so the caller can filter, search and page locally
    /// (this is what Argosy does too). `progress(loaded, total)` is called after every page;
    /// `total` is known from the first page on. Files are left out: only an install needs
    /// them, and it fetches the ROM itself.
    pub async fn library(&self, progress: &mut (dyn FnMut(usize, Option<i64>) + Send)) -> Result<Vec<Rom>> {
        const PAGE: i64 = 500;
        let mut out: Vec<Rom> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let (mut fetched, mut total): (i64, Option<i64>) = (0, None);
        loop {
            let mut q: Vec<(&str, String)> = vec![
                ("limit", PAGE.to_string()),
                ("offset", fetched.to_string()),
                // Name order with RomM's id tiebreak: stable, so paging can't skip or repeat rows.
                ("order_by", "name".into()),
                ("group_by_meta_id", "true".into()),
                ("with_files", "false".into()),
                ("with_char_index", "false".into()),
                ("with_filter_values", "false".into()),
                ("with_rom_id_index", "false".into()),
            ];
            // The count is its own scan; once is enough.
            if fetched > 0 {
                q.push(("with_total", "false".into()));
            }
            let page: Page<Rom> = self.get_json(&["roms"], &q).await?;
            total = total.or(page.total);
            let got = page.items.len() as i64;
            fetched += got;
            // A library edited mid-fetch can shift rows between pages; never list a game twice.
            out.extend(page.items.into_iter().filter(|r| seen.insert(r.id)));
            progress(out.len(), total);
            if got < PAGE || total.is_some_and(|t| fetched >= t) {
                break;
            }
        }
        Ok(out)
    }

    /// Pages through the whole result set.
    pub async fn all_roms(&self, platform_id: Option<i64>, search: Option<&str>) -> Result<Vec<Rom>> {
        const PAGE: i64 = 500;
        let mut out = Vec::new();
        loop {
            let page = self.roms(platform_id, search, PAGE, out.len() as i64).await?;
            let got = page.items.len() as i64;
            out.extend(page.items);
            if got < PAGE {
                break;
            }
        }
        Ok(out)
    }

    pub async fn rom(&self, id: i64) -> Result<Rom> {
        self.get_json(&["roms", &id.to_string()], &[]).await
    }

    /// The size of each of `ids`, in the order given, each once. For the versions of a game:
    /// RomM's `sibling_roms` names them but says nothing about their size, and the server can't
    /// be asked for several ROMs at once, so this is one request each. A ROM that can't be read
    /// (removed meanwhile, a server hiccup) is left out rather than failing the others.
    pub async fn rom_sizes(&self, ids: &[i64]) -> Vec<RomSize> {
        /// More than any game has versions; a bound on what one call can cost.
        const MAX: usize = 32;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for &id in ids.iter().filter(|id| seen.insert(**id)).take(MAX) {
            if let Ok(rom) = self.rom(id).await {
                out.push(RomSize { id, bytes: rom.fs_size_bytes, multi_file: rom.has_multiple_files });
            }
        }
        out
    }

    /// Streams a ROM to `dest`. Single-file ROMs arrive as the file itself;
    /// multi-file ROMs arrive as a zip (RomM includes an .m3u where applicable).
    pub async fn download_rom(
        &self,
        rom: &Rom,
        dest: &Path,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<()> {
        let id = rom.id.to_string();
        let mut url = self.url(&["roms", &id, "content", &rom.fs_name])?;
        let ids: Vec<String> = rom.files.iter().map(|f| f.id.to_string()).collect();
        if rom.has_multiple_files && !ids.is_empty() {
            url.query_pairs_mut().append_pair("file_ids", &ids.join(","));
        }
        self.download_url(url, dest, progress).await
    }

    pub async fn download_firmware(
        &self,
        fw: &Firmware,
        dest: &Path,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<()> {
        let url = self.url(&["firmware", &fw.id.to_string(), "content", &fw.file_name])?;
        self.download_url(url, dest, progress).await
    }

    /// Download to `<dest>.part`, then rename, so an interrupted download never
    /// leaves a truncated file under the real name.
    async fn download_url(&self, url: Url, dest: &Path, progress: &mut (dyn FnMut(u64, Option<u64>) + Send)) -> Result<()> {
        let resp = Self::check(self.send(self.authed(self.http.get(url))).await?).await?;
        let total = resp.content_length();
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut part = dest.as_os_str().to_owned();
        part.push(".part");
        let part = std::path::PathBuf::from(part);

        let result: Result<()> = async {
            let mut file = tokio::fs::File::create(&part).await?;
            let mut done = 0u64;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                file.write_all(&chunk).await?;
                done += chunk.len() as u64;
                progress(done, total);
            }
            file.flush().await?;
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                tokio::fs::rename(&part, dest).await?;
                Ok(())
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                Err(e)
            }
        }
    }

    // --------------------------------------------------------------- sync

    /// Idempotent: RomM returns the existing device when `hostname`+`platform` match.
    pub async fn register_device(&self, reg: &DeviceRegistration) -> Result<Device> {
        let resp = self.send(self.authed(self.http.post(self.url(&["devices"])?).json(reg))).await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn negotiate(&self, req: &NegotiateRequest) -> Result<NegotiateResponse> {
        let resp = self.send(self.authed(self.http.post(self.url(&["sync", "negotiate"])?).json(req))).await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn complete_session(&self, session_id: i64, req: &CompleteRequest) -> Result<()> {
        let url = self.url(&["sync", "sessions", &session_id.to_string(), "complete"])?;
        let resp = self.send(self.authed(self.http.post(url).json(req))).await?;
        Self::check(resp).await?;
        Ok(())
    }

    fn upload_query(p: &UploadParams) -> Vec<(&'static str, String)> {
        let mut q: Vec<(&'static str, String)> = Vec::new();
        if let Some(e) = &p.emulator {
            q.push(("emulator", e.clone()));
        }
        if let Some(s) = &p.slot {
            q.push(("slot", s.clone()));
        }
        if let Some(d) = &p.device_id {
            q.push(("device_id", d.clone()));
        }
        if let Some(s) = p.session_id {
            q.push(("session_id", s.to_string()));
        }
        if let Some(h) = &p.content_hash {
            q.push(("content_hash", h.clone()));
        }
        q
    }

    /// New save. `overwrite=false`, so a slot someone else filled returns 409
    /// instead of being clobbered; callers should treat 409 as a conflict.
    pub async fn upload_save(&self, p: &UploadParams, bytes: Vec<u8>) -> Result<Save> {
        let mut q = Self::upload_query(p);
        q.push(("rom_id", p.rom_id.to_string()));
        q.push(("overwrite", "false".into()));
        q.push(("autocleanup", "true".into()));
        q.push(("autocleanup_limit", "10".into()));
        let part = multipart::Part::bytes(bytes).file_name(p.file_name.clone());
        let form = multipart::Form::new().part("saveFile", part);
        let resp = self
            .send(self.authed(self.http.post(self.url(&["saves"])?).query(&q).multipart(form)))
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// Replace a save this device created. RomM applies no conflict guard here.
    pub async fn replace_save(&self, save_id: i64, p: &UploadParams, bytes: Vec<u8>) -> Result<Save> {
        let q = Self::upload_query(p);
        let part = multipart::Part::bytes(bytes).file_name(p.file_name.clone());
        let form = multipart::Form::new().part("saveFile", part);
        let resp = self
            .send(self.authed(self.http.put(self.url(&["saves", &save_id.to_string()])?).query(&q).multipart(form)))
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// Fetch a server-relative resource such as cover art (`/assets/romm/resources/...`).
    pub async fn fetch_asset(&self, path: &str) -> Result<(Vec<u8>, Option<String>)> {
        let rel = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
        let url = Url::parse(&format!("{}{}", self.base, rel)).map_err(|e| Error::msg(e.to_string()))?;
        let resp = Self::check(self.authed(self.http.get(url)).send().await?).await?;
        let ctype = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
        Ok((resp.bytes().await?.to_vec(), ctype))
    }

    // ------------------------------------------------------------- states

    pub async fn list_states(&self, rom_id: i64) -> Result<Vec<StateInfo>> {
        self.get_json(&["states"], &[("rom_id", rom_id.to_string())]).await
    }

    /// Create or replace a state. Without `existing_id` this is `POST /api/states`, which RomM
    /// treats as an upsert on (user, ROM, file name). With it, `PUT /api/states/{id}`.
    /// The file is streamed from disk rather than read into memory (states can be 100+ MB).
    pub async fn upload_state(
        &self,
        rom_id: i64,
        emulator: &str,
        existing_id: Option<i64>,
        name: &str,
        path: &Path,
        screenshot: Option<&Path>,
    ) -> Result<StateInfo> {
        async fn file_part(path: &Path, name: &str) -> Result<multipart::Part> {
            let file = tokio::fs::File::open(path).await?;
            let len = file.metadata().await?.len();
            let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
            Ok(multipart::Part::stream_with_length(body, len).file_name(name.to_string()))
        }
        let mut form = multipart::Form::new().part("stateFile", file_part(path, name).await?);
        if let Some(png) = screenshot {
            let shot_name = format!("{name}.png");
            form = form.part("screenshotFile", file_part(png, &shot_name).await?);
        }
        let rb = match existing_id {
            Some(id) => self.http.put(self.url(&["states", &id.to_string()])?),
            None => self
                .http
                .post(self.url(&["states"])?)
                .query(&[("rom_id", rom_id.to_string()), ("emulator", emulator.to_string())]),
        };
        let resp = self.send(self.authed(rb.multipart(form))).await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn download_state(
        &self,
        id: i64,
        dest: &Path,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<()> {
        let url = self.url(&["states", &id.to_string(), "content"])?;
        self.download_url(url, dest, progress).await
    }

    pub async fn delete_states(&self, ids: &[i64]) -> Result<()> {
        let resp = self
            .send(self.authed(self.http.post(self.url(&["states", "delete"])?).json(&serde_json::json!({ "states": ids }))))
            .await?;
        Self::check(resp).await?;
        Ok(())
    }

    // ------------------------------------------------------ RetroAchievements

    /// The signed-in user (`me.read`). This carries the user's *entire* RetroAchievements
    /// progression, which is large for heavy RA users, so callers should cache it.
    pub async fn me(&self) -> Result<Me> {
        self.get_json(&["users", "me"], &[]).await
    }

    /// Only the achievement fields of one ROM (the full ROM response is much bigger).
    pub async fn rom_ra(&self, id: i64) -> Result<RomRa> {
        self.get_json(&["roms", &id.to_string()], &[]).await
    }

    /// Ask RomM to re-pull the user's progress from retroachievements.org.
    ///
    /// Needs the `me.write` scope, which the app doesn't otherwise request (it also allows editing
    /// the profile), so a 403 here is expected for most tokens. `incremental` only re-fetches games
    /// whose award count or last-earned date changed, which is what you want after playing.
    pub async fn refresh_ra(&self, user_id: i64, incremental: bool) -> Result<()> {
        let url = self.url(&["users", &user_id.to_string(), "ra", "refresh"])?;
        let resp = self.authed(self.http.post(url).json(&serde_json::json!({ "incremental": incremental }))).send().await?;
        Self::check(resp).await?;
        Ok(())
    }

    pub async fn download_save(&self, save_id: i64, device_id: &str, session_id: i64) -> Result<Vec<u8>> {
        let q = [
            ("device_id", device_id.to_string()),
            ("session_id", session_id.to_string()),
            ("optimistic", "true".to_string()),
        ];
        let url = self.url(&["saves", &save_id.to_string(), "content"])?;
        let resp = self.send(self.authed(self.http.get(url).query(&q))).await?;
        Ok(Self::check(resp).await?.bytes().await?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls() {
        assert_eq!(normalize_base_url("192.168.1.10:8080").unwrap(), "http://192.168.1.10:8080");
        assert_eq!(normalize_base_url("https://romm.example.com/").unwrap(), "https://romm.example.com");
        assert!(normalize_base_url("  ").is_err());
    }

    #[test]
    fn encodes_path_segments() {
        let c = RommClient::new("http://h", None, false).unwrap();
        let u = c.url(&["roms", "1", "content", "Zelda: A Link/Between #1.zip"]).unwrap();
        assert!(u.as_str().ends_with("/api/roms/1/content/Zelda:%20A%20Link%2FBetween%20%231.zip"), "{u}");
    }

    #[test]
    fn keeps_base_path_prefix() {
        let c = RommClient::new("https://example.com/romm", None, false).unwrap();
        assert_eq!(c.url(&["platforms"]).unwrap().as_str(), "https://example.com/romm/api/platforms");
    }
}
