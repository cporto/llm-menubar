use std::process::Command;
use std::sync::Mutex;
use anyhow::{Result, Context};
use log::{info, warn};

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub loaded: bool,
}

pub struct OmlxManager {
    api_url: String,
    api_key: String,
    service_label: String,
    plist_path: String,
    admin_session: Mutex<Option<String>>,
}

#[derive(Clone, serde::Serialize)]
pub struct OmlxStatus {
    pub running: bool,
    pub model: String,
    pub pid: Option<u32>,
}

impl OmlxManager {
    pub fn new(api_url: &str, api_key: &str, service_label: &str, plist_path: &str) -> Self {
        Self {
            api_url: api_url.to_string(),
            api_key: api_key.to_string(),
            service_label: service_label.to_string(),
            plist_path: plist_path.to_string(),
            admin_session: Mutex::new(None),
        }
    }

    fn base_url(&self) -> &str {
        self.api_url.strip_suffix("/v1").unwrap_or(&self.api_url)
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap()
    }

    fn uid() -> String {
        let output = Command::new("id").arg("-u").output().expect("failed to get uid");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    pub fn start(&self) -> Result<()> {
        let uid = Self::uid();
        let domain = format!("gui/{}", uid);
        info!("Starting oMLX via launchctl...");
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
        info!("oMLX service started");
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        let uid = Self::uid();
        let target = format!("gui/{}/{}", uid, self.service_label);
        info!("Stopping oMLX via launchctl...");
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
        *self.admin_session.lock().unwrap() = None;
        info!("oMLX service stopped");
        Ok(())
    }

    pub fn restart(&self) -> Result<()> {
        self.stop()?;
        std::thread::sleep(std::time::Duration::from_secs(1));
        self.start()
    }

    pub async fn check_health(&self) -> OmlxStatus {
        // Quick check: is the server responding at all?
        let client = Self::client();
        let mut req = client.get(format!("{}/models", self.api_url));
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let running = match req.send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        };
        if !running {
            return OmlxStatus { running: false, model: String::new(), pid: None };
        }

        // Use admin API to find which model is actually loaded.
        let model = match self.list_models().await {
            Ok(models) => models.iter()
                .find(|m| m.loaded)
                .map(|m| m.id.clone())
                .unwrap_or_default(),
            Err(_) => String::new(),
        };

        OmlxStatus { running: true, model, pid: None }
    }

    // Admin API: login and cache the session cookie.
    async fn admin_login(&self) -> Result<String> {
        {
            let cached = self.admin_session.lock().unwrap();
            if let Some(ref s) = *cached {
                return Ok(s.clone());
            }
        }

        let client = Self::client();
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

    /// Fetch available models with their loaded state.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let cookie = self.admin_login().await?;
        let client = Self::client();
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

    /// Load a model (unloads any currently loaded model first via oMLX's LRU).
    pub async fn load_model(&self, model_id: &str) -> Result<()> {
        let cookie = self.admin_login().await?;
        let client = Self::client();
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
