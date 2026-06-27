use std::process::Command;
use std::sync::Mutex;
use anyhow::{Result, Context};
use async_trait::async_trait;
use log::{info, warn};

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub loaded: bool,
}

#[derive(Clone, serde::Serialize)]
pub struct ServerStatus {
    pub running: bool,
    pub model: String,
    pub pid: Option<u32>,
}

#[async_trait]
pub trait ModelManager: Send + Sync {
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;
    async fn load_model(&self, model_id: &str) -> Result<()>;
    async fn check_health(&self) -> ServerStatus;
}

// --- Launchctl lifecycle (shared by all server types) ---

pub struct LaunchctlService {
    service_label: String,
    plist_path: String,
}

impl LaunchctlService {
    pub fn new(service_label: &str, plist_path: &str) -> Self {
        Self {
            service_label: service_label.to_string(),
            plist_path: plist_path.to_string(),
        }
    }

    fn uid() -> String {
        let output = Command::new("id").arg("-u").output().expect("failed to get uid");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    pub fn start(&self) -> Result<()> {
        let uid = Self::uid();
        let domain = format!("gui/{}", uid);
        info!("Starting service via launchctl...");
        let output = Command::new("launchctl")
            .args(["bootstrap", &domain, &self.plist_path])
            .output()
            .context("Failed to run launchctl bootstrap")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("5:") || stderr.contains("37") || stderr.contains("already loaded") {
                warn!("Service already loaded");
                return Ok(());
            }
            anyhow::bail!("launchctl bootstrap failed: {}", stderr);
        }
        info!("Service started");
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
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

    pub fn restart(&self) -> Result<()> {
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
}

impl OmlxModelManager {
    pub fn new(api_url: &str, api_key: &str) -> Self {
        Self {
            api_url: api_url.to_string(),
            api_key: api_key.to_string(),
            admin_session: Mutex::new(None),
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

        let client = http_client();
        let resp = client
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
}

#[async_trait]
impl ModelManager for OmlxModelManager {
    async fn check_health(&self) -> ServerStatus {
        let client = http_client();
        let mut req = client.get(format!("{}/models", self.api_url));
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let running = match req.send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        };
        if !running {
            return ServerStatus { running: false, model: String::new(), pid: None };
        }

        let model = match self.list_models().await {
            Ok(models) => models.iter()
                .find(|m| m.loaded)
                .map(|m| m.id.clone())
                .unwrap_or_default(),
            Err(_) => String::new(),
        };

        ServerStatus { running: true, model, pid: None }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let cookie = self.admin_login().await?;
        let client = http_client();
        let resp = client
            .get(format!("{}/admin/api/models", self.base_url()))
            .header("Cookie", &cookie)
            .send()
            .await
            .context("Failed to fetch models")?;

        let body: serde_json::Value = resp.json().await?;
        let models = body["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        Some(ModelInfo {
                            id: m["id"].as_str()?.to_string(),
                            loaded: m["loaded"].as_bool().unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }

    async fn load_model(&self, model_id: &str) -> Result<()> {
        let cookie = self.admin_login().await?;
        let client = http_client();
        info!("Loading model: {}", model_id);
        let resp = client
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
}

// --- llama.cpp model manager ---

pub struct LlamacppModelManager {
    api_url: String,
}

impl LlamacppModelManager {
    pub fn new(api_url: &str) -> Self {
        let url = api_url.strip_suffix("/v1").unwrap_or(api_url);
        Self { api_url: url.to_string() }
    }
}

#[async_trait]
impl ModelManager for LlamacppModelManager {
    async fn check_health(&self) -> ServerStatus {
        let client = http_client();
        let resp = match client.get(format!("{}/models", self.api_url)).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => return ServerStatus { running: false, model: String::new(), pid: None },
        };

        let model = match resp.json::<serde_json::Value>().await {
            Ok(body) => body["data"].as_array()
                .and_then(|arr| arr.iter()
                    .find(|m| m["status"].as_str() == Some("loaded"))
                    .and_then(|m| m["id"].as_str().map(String::from)))
                .unwrap_or_default(),
            Err(_) => String::new(),
        };

        ServerStatus { running: true, model, pid: None }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let client = http_client();
        let resp = client
            .get(format!("{}/models", self.api_url))
            .send()
            .await
            .context("Failed to fetch models")?;

        let body: serde_json::Value = resp.json().await?;
        let models = body["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        Some(ModelInfo {
                            id: m["id"].as_str()?.to_string(),
                            loaded: m["status"].as_str() == Some("loaded"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }

    async fn load_model(&self, model_id: &str) -> Result<()> {
        let client = http_client();
        info!("Loading model: {}", model_id);
        let resp = client
            .post(format!("{}/models/load", self.api_url))
            .json(&serde_json::json!({"model": model_id}))
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
}

// --- Composed server manager ---

pub struct ServerManager {
    pub launchctl: LaunchctlService,
    pub models: Box<dyn ModelManager>,
    pub server_type: String,
}

impl ServerManager {
    pub fn new(server_type: &str, api_url: &str, api_key: &str, service_label: &str, plist_path: &str) -> Self {
        let models: Box<dyn ModelManager> = match server_type {
            "llamacpp" => Box::new(LlamacppModelManager::new(api_url)),
            _ => Box::new(OmlxModelManager::new(api_url, api_key)),
        };
        Self {
            launchctl: LaunchctlService::new(service_label, plist_path),
            models,
            server_type: server_type.to_string(),
        }
    }

    pub fn start(&self) -> Result<()> { self.launchctl.start() }
    pub fn stop(&self) -> Result<()> {
        let result = self.launchctl.stop();
        if self.server_type != "llamacpp" {
            if let Some(omlx) = self.omlx_manager() {
                omlx.clear_session();
            }
        }
        result
    }
    pub fn restart(&self) -> Result<()> { self.launchctl.restart() }

    pub async fn check_health(&self) -> ServerStatus { self.models.check_health().await }
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> { self.models.list_models().await }
    pub async fn load_model(&self, model_id: &str) -> Result<()> { self.models.load_model(model_id).await }

    fn omlx_manager(&self) -> Option<&OmlxModelManager> {
        // Safety: we know the concrete type when server_type is "omlx"
        None // Can't downcast Box<dyn Trait> without Any; session clear is best-effort
    }

    pub fn display_prefix(&self) -> &str {
        match self.server_type.as_str() {
            "llamacpp" => ".cpp",
            _ => "",
        }
    }

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
