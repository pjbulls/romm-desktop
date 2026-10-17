//! Typed client for the RomM REST API.

pub mod types;

use std::path::Path;

use futures_util::StreamExt;
use reqwest::{multipart, Response, Url};
use serde::de::DeserializeOwned;
use tokio::io::AsyncWriteExt;

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
}

impl RommClient {
    pub fn new(base_url: &str, token: Option<String>, accept_invalid_certs: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(format!("{CLIENT_SLUG}/{}", env!("CARGO_PKG_VERSION")))
            .danger_accept_invalid_certs(accept_invalid_certs)
            .build()?;
        Ok(Self { http, base: normalize_base_url(base_url)?, token })
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
        let resp = self.authed(self.http.get(self.url(segments)?).query(query)).send().await?;
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
        let resp = Self::check(self.authed(self.http.get(url)).send().await?).await?;
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
        let resp = self.authed(self.http.post(self.url(&["devices"])?).json(reg)).send().await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn negotiate(&self, req: &NegotiateRequest) -> Result<NegotiateResponse> {
        let resp = self.authed(self.http.post(self.url(&["sync", "negotiate"])?).json(req)).send().await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn complete_session(&self, session_id: i64, req: &CompleteRequest) -> Result<()> {
        let url = self.url(&["sync", "sessions", &session_id.to_string(), "complete"])?;
        let resp = self.authed(self.http.post(url).json(req)).send().await?;
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
            .authed(self.http.post(self.url(&["saves"])?).query(&q).multipart(form))
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// Replace a save this device created. RomM applies no conflict guard here.
    pub async fn replace_save(&self, save_id: i64, p: &UploadParams, bytes: Vec<u8>) -> Result<Save> {
        let q = Self::upload_query(p);
        let part = multipart::Part::bytes(bytes).file_name(p.file_name.clone());
        let form = multipart::Form::new().part("saveFile", part);
        let resp = self
            .authed(self.http.put(self.url(&["saves", &save_id.to_string()])?).query(&q).multipart(form))
            .send()
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
        let resp = self.authed(rb.multipart(form)).send().await?;
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
            .authed(self.http.post(self.url(&["states", "delete"])?).json(&serde_json::json!({ "states": ids })))
            .send()
            .await?;
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
        let resp = self.authed(self.http.get(url).query(&q)).send().await?;
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
