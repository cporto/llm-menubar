use std::process::Command;
use std::sync::Mutex;
use anyhow::{Result, Context};
use async_trait::async_trait;
use log::{info, warn};

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    pub loaded: bool,
}

#[derive(Clone, serde::Serialize)]
pub struct ServerStatus {
    pub running: bool,
    pub model: String,
}

#[async_trait]
pub trait ModelManager: Send + Sync {
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;
    async fn load_model(&self, model_id: &str) -> Result<()>;
    async fn unload_model(&self, model_id: &str) -> Result<()>;
    async fn check_health(&self) -> ServerStatus;
    fn on_stop(&self) {}
}

// --- Service lifecycle control ---
//
// Abstracted behind a trait so a future remote (SSH) implementation can slot
// in beside the local launchctl one without touching the rest of the app.
pub trait ServiceControl: Send + Sync {
    fn start(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
    fn restart(&self) -> Result<()>;
}

// --- Launchctl lifecycle (local, per-user launchd agent) ---

pub struct LaunchctlService {
    service_label: String,
    plist_path: String,
}

impl LaunchctlService {
    pub fn new(service_label: &str, plist_path: &str) -> Self {
        Self {
            service_label: service_label.to_string(),
            plist_path: expand_tilde(plist_path),
        }
    }

    fn uid() -> String {
        let output = Command::new("id").arg("-u").output().expect("failed to get uid");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

impl ServiceControl for LaunchctlService {
    fn start(&self) -> Result<()> {
        let uid = Self::uid();
        let domain = format!("gui/{}", uid);
        let target = format!("gui/{}/{}", uid, self.service_label);

        info!("Starting service via launchctl...");
        let output = Command::new("launchctl")
            .args(["bootstrap", &domain, &self.plist_path])
            .output()
            .context("Failed to run launchctl bootstrap")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("5:") && !stderr.contains("37") && !stderr.contains("already loaded") {
                anyhow::bail!("launchctl bootstrap failed: {}", stderr);
            }
            info!("Service already loaded, will kickstart");
        }

        let kick = Command::new("launchctl")
            .args(["kickstart", "-k", &target])
            .output()
            .context("Failed to run launchctl kickstart")?;

        if !kick.status.success() {
            let stderr = String::from_utf8_lossy(&kick.stderr);
            warn!("launchctl kickstart: {}", stderr);
        }

        info!("Service started");
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        let uid = Self::uid();
        let target = format!("gui/{}/{}", uid, self.service_label);
        info!("Stopping service via launchctl...");
        let output = Command::new("launchctl")
            .args(["bootout", &target])
            .output()
            .context("Failed to run launchctl bootout")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("3") || stderr.contains("could not find") {
                warn!("Service not loaded");
                return Ok(());
            }
            anyhow::bail!("launchctl bootout failed: {}", stderr);
        }
        info!("Service stopped");
        Ok(())
    }

    fn restart(&self) -> Result<()> {
        self.stop()?;
        std::thread::sleep(std::time::Duration::from_secs(1));
        self.start()
    }
}

// --- oMLX model manager ---

pub struct OmlxModelManager {
    api_url: String,
    api_key: String,
    admin_session: Mutex<Option<String>>,
    client: reqwest::Client,
}

impl OmlxModelManager {
    pub fn new(api_url: &str, api_key: &str) -> Self {
        Self {
            api_url: api_url.to_string(),
            api_key: api_key.to_string(),
            admin_session: Mutex::new(None),
            client: http_client(),
        }
    }

    fn base_url(&self) -> &str {
        self.api_url.strip_suffix("/v1").unwrap_or(&self.api_url)
    }

    async fn admin_login(&self) -> Result<String> {
        {
            let cached = self.admin_session.lock().unwrap();
            if let Some(ref s) = *cached {
                return Ok(s.clone());
            }
        }

        let resp = self.client
            .post(format!("{}/admin/api/login", self.base_url()))
            .json(&serde_json::json!({"api_key": self.api_key}))
            .send()
            .await
            .context("Admin login request failed")?;

        let cookie = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .find_map(|v| {
                let s = v.to_str().ok()?;
                if s.starts_with("omlx_admin_session=") {
                    Some(s.split(';').next().unwrap_or(s).to_string())
                } else {
                    None
                }
            })
            .context("No admin session cookie in login response")?;

        let mut cached = self.admin_session.lock().unwrap();
        *cached = Some(cookie.clone());
        Ok(cookie)
    }

    pub fn clear_session(&self) {
        *self.admin_session.lock().unwrap() = None;
    }

    fn parse_model_list(&self, body: &serde_json::Value) -> Result<Vec<ModelInfo>> {
        Ok(body["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m["id"].as_str()?.to_string();
                        let label = id.clone();
                        Some(ModelInfo { id, label, loaded: m["loaded"].as_bool().unwrap_or(false) })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[async_trait]
impl ModelManager for OmlxModelManager {
    async fn check_health(&self) -> ServerStatus {
        let mut req = self.client.get(format!("{}/models", self.api_url));
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let running = match req.send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        };
        if !running {
            return ServerStatus { running: false, model: String::new() };
        }

        let model = match self.list_models().await {
            Ok(models) => models.iter()
                .find(|m| m.loaded)
                .map(|m| m.label.clone())
                .unwrap_or_default(),
            Err(_) => String::new(),
        };

        ServerStatus { running: true, model }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let cookie = self.admin_login().await?;
        let resp = self.client
            .get(format!("{}/admin/api/models", self.base_url()))
            .header("Cookie", &cookie)
            .send()
            .await
            .context("Failed to fetch models")?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED || resp.status() == reqwest::StatusCode::FORBIDDEN {
            self.clear_session();
            let cookie = self.admin_login().await?;
            let resp = self.client
                .get(format!("{}/admin/api/models", self.base_url()))
                .header("Cookie", &cookie)
                .send()
                .await
                .context("Failed to fetch models (retry)")?;
            let body: serde_json::Value = resp.json().await?;
            return self.parse_model_list(&body);
        }

        let body: serde_json::Value = resp.json().await?;
        self.parse_model_list(&body)
    }

    async fn load_model(&self, model_id: &str) -> Result<()> {
        let cookie = self.admin_login().await?;
        info!("Loading model: {}", model_id);
        let resp = self.client
            .post(format!("{}/admin/api/models/{}/load", self.base_url(), model_id))
            .header("Cookie", &cookie)
            .send()
            .await
            .context("Failed to load model")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Model load failed: {}", body);
        }
        info!("Model loaded: {}", model_id);
        Ok(())
    }

    async fn unload_model(&self, _model_id: &str) -> Result<()> {
        Ok(())
    }

    fn on_stop(&self) {
        self.clear_session();
    }
}

// --- llama.cpp model manager ---

pub struct LlamacppModelManager {
    api_url: String,
    client: reqwest::Client,
}

impl LlamacppModelManager {
    pub fn new(api_url: &str) -> Self {
        let url = api_url.strip_suffix("/v1").unwrap_or(api_url);
        Self { api_url: url.to_string(), client: http_client() }
    }

    /// Extract the loaded/unloaded state from a model entry's `status` field.
    /// Router mode returns either a plain string or an object with a `value` key.
    fn parse_status(entry: &serde_json::Value) -> Option<bool> {
        if let Some(s) = entry["status"].as_str() {
            return Some(s == "loaded");
        }
        if let Some(s) = entry["status"]["value"].as_str() {
            return Some(s == "loaded");
        }
        None
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>> {
        if let Ok(resp) = self.client.get(format!("{}/models", self.api_url)).send().await {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(arr) = body["data"].as_array() {
                        if arr.iter().any(|m| Self::parse_status(m).is_some()) {
                            info!("llama.cpp router mode detected ({} models)", arr.len());
                            return Ok(arr.iter()
                                .filter_map(|m| {
                                    let id = m["id"].as_str()?.to_string();
                                    let label = display_id(&id);
                                    Some(ModelInfo { id, label, loaded: Self::parse_status(m).unwrap_or(false) })
                                })
                                .collect());
                        }
                        info!("llama.cpp single-model mode ({} entries)", arr.len());
                        return Ok(arr.iter()
                            .filter_map(|m| {
                                let id = m["id"].as_str()?.to_string();
                                let label = display_id(&id);
                                Some(ModelInfo { id, label, loaded: true })
                            })
                            .collect());
                    }
                }
            }
        }

        info!("llama.cpp /models unavailable, trying /v1/models fallback");
        let resp = self.client
            .get(format!("{}/v1/models", self.api_url))
            .send()
            .await
            .context("Failed to fetch models")?;
        let body: serde_json::Value = resp.json().await?;
        let models = body["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m["id"].as_str()?.to_string();
                        let label = display_id(&id);
                        Some(ModelInfo { id, label, loaded: true })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(models)
    }
}

#[async_trait]
impl ModelManager for LlamacppModelManager {
    async fn check_health(&self) -> ServerStatus {
        let running = match self.client.get(format!("{}/health", self.api_url)).send().await {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        };
        if !running {
            return ServerStatus { running: false, model: String::new() };
        }
        let model = match self.fetch_models().await {
            Ok(models) => models.iter()
                .find(|m| m.loaded)
                .map(|m| m.label.clone())
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
        ServerStatus { running: true, model }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        self.fetch_models().await
    }

    async fn load_model(&self, model_id: &str) -> Result<()> {
        info!("Loading model: {}", model_id);
        let resp = self.client
            .post(format!("{}/models/load", self.api_url))
            .json(&serde_json::json!({"model": model_id}))
            .send()
            .await
            .context("Failed to load model")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("already running") {
                return Ok(());
            }
            anyhow::bail!("Model load failed: {}", body);
        }
        info!("Model loaded: {}", model_id);
        Ok(())
    }

    async fn unload_model(&self, model_id: &str) -> Result<()> {
        info!("Unloading model: {}", model_id);
        let resp = self.client
            .post(format!("{}/models/unload", self.api_url))
            .json(&serde_json::json!({"model": model_id}))
            .send()
            .await
            .context("Failed to unload model")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("not running") {
                return Ok(());
            }
            anyhow::bail!("Model unload failed: {}", body);
        }
        info!("Model unloaded: {}", model_id);
        Ok(())
    }
}

// --- Composed server manager ---

pub struct ServerManager {
    pub control: Box<dyn ServiceControl>,
    pub models: Box<dyn ModelManager>,
    pub server_type: String,
    pub is_local: bool,
}

impl ServerManager {
    pub fn new(server_type: &str, api_url: &str, api_key: &str, service_label: &str, plist_path: &str) -> Self {
        let models: Box<dyn ModelManager> = match server_type {
            "llamacpp" => Box::new(LlamacppModelManager::new(api_url)),
            _ => Box::new(OmlxModelManager::new(api_url, api_key)),
        };
        Self {
            control: Box::new(LaunchctlService::new(service_label, plist_path)),
            models,
            server_type: server_type.to_string(),
            is_local: is_local_host(api_url),
        }
    }

    pub fn start(&self) -> Result<()> { self.control.start() }
    pub fn stop(&self) -> Result<()> {
        let result = self.control.stop();
        self.models.on_stop();
        result
    }
    pub fn restart(&self) -> Result<()> { self.control.restart() }

    pub async fn check_health(&self) -> ServerStatus { self.models.check_health().await }
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> { self.models.list_models().await }
    pub async fn load_model(&self, model_id: &str) -> Result<()> { self.models.load_model(model_id).await }
    pub async fn unload_model(&self, model_id: &str) -> Result<()> { self.models.unload_model(model_id).await }

    pub fn display_name(&self) -> &str {
        match self.server_type.as_str() {
            "llamacpp" => "llama.cpp",
            _ => "oMLX",
        }
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
}

/// Clean up a model id for display. Handles:
/// - Full gguf paths → basename without extension
/// - HuggingFace repo IDs like "bartowski/mistralai_Model-Name-GGUF:Q4_K_M"
///   → "Model-Name"
fn display_id(raw: &str) -> String {
    // HF repo IDs look like "user/org_Model-GGUF:quant"; filesystem paths start with /
    let is_hf_repo = raw.contains('/') && !raw.starts_with('/');
    let mut name = raw;
    // Strip directory/repo path
    if let Some(after) = name.rsplit('/').next() { name = after; }
    // Strip quant suffix after : (e.g. ":Q4_K_M")
    if let Some(before) = name.split(':').next() { name = before; }
    // Strip publisher prefix (e.g. "mistralai_Model-Name") — only for HF repo IDs
    if is_hf_repo {
        if let Some(idx) = name.find('_') {
            let candidate = &name[idx + 1..];
            if !candidate.is_empty() { name = candidate; }
        }
    }
    // Strip -GGUF / .gguf suffixes
    let name = name.strip_suffix("-GGUF").or_else(|| name.strip_suffix("-gguf")).unwrap_or(name);
    let name = name.strip_suffix(".gguf").unwrap_or(name);
    name.to_string()
}

/// Expand a leading `~/` to the user's home dir. launchctl receives the plist
/// path as a raw argument and does not perform shell tilde expansion.
pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

/// Is the configured server on this machine? launchctl can only control local
/// per-user agents, so Start/Stop is gated on this. Remote control (SSH) is a
/// future upgrade path.
fn is_local_host(api_url: &str) -> bool {
    let host = api_url
        .split("://")
        .nth(1)
        .unwrap_or(api_url)
        .split('/')
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    // Strip port (handle IPv6 [::1]:port too).
    let host = if let Some(stripped) = host.strip_prefix('[') {
        stripped.split(']').next().unwrap_or(stripped)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0" | "")
}
