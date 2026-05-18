use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

fn default_delete_split_pdfs() -> bool {
    true
}

fn default_use_source_dir_as_output() -> bool {
    false
}

fn default_delete_original_files_after_done() -> bool {
    false
}

fn default_api_request_pool_size() -> u32 {
    10
}

fn default_pdf_split_pages() -> u32 {
    100
}

fn default_is_ocr() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiProfile {
    pub id: String,
    pub name: String,
    pub token: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub apis: Vec<ApiProfile>,
    #[serde(default)]
    pub active_api_id: Option<String>,
    pub output_dir: Option<String>,
    #[serde(default = "default_delete_split_pdfs")]
    pub delete_split_pdfs_after_done: bool,
    #[serde(default = "default_use_source_dir_as_output")]
    pub use_source_dir_as_output: bool,
    #[serde(default = "default_delete_original_files_after_done")]
    pub delete_original_files_after_done: bool,
    #[serde(default = "default_api_request_pool_size")]
    pub api_request_pool_size: u32,
    #[serde(default = "default_pdf_split_pages")]
    pub pdf_split_pages: u32,
    #[serde(default)]
    pub balance_load_across_apis: bool,
    #[serde(default = "default_is_ocr")]
    pub is_ocr: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            apis: Vec::new(),
            active_api_id: None,
            output_dir: None,
            delete_split_pdfs_after_done: true,
            use_source_dir_as_output: false,
            delete_original_files_after_done: false,
            api_request_pool_size: default_api_request_pool_size(),
            pdf_split_pages: default_pdf_split_pages(),
            balance_load_across_apis: false,
            is_ocr: true,
        }
    }
}

impl AppSettings {
    pub fn normalized(mut self) -> Self {
        self.api_request_pool_size = self.api_request_pool_size.clamp(1, 100);
        self.pdf_split_pages = self.pdf_split_pages.clamp(1, 1000);
        self
    }
}

pub fn config_dir() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("mineru-cli");
    fs::create_dir_all(&path).ok();
    path
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn load_settings() -> Result<AppSettings, String> {
    let path = config_path();
    if path.exists() {
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("读取配置失败 {}: {}", path.display(), e))?;
        let settings: AppSettings = toml::from_str(&content)
            .map_err(|e| format!("解析配置失败 {}: {}", path.display(), e))?;
        return Ok(settings.normalized());
    }

    Ok(AppSettings::default())
}

pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    let path = config_path();
    let content = toml::to_string_pretty(&settings.clone().normalized())
        .map_err(|e| format!("序列化配置失败: {}", e))?;
    fs::write(&path, content).map_err(|e| format!("保存配置失败 {}: {}", path.display(), e))
}

pub fn active_profile(settings: &AppSettings) -> Option<ApiProfile> {
    let active = settings.active_api_id.as_deref();
    active
        .and_then(|id| settings.apis.iter().find(|p| p.id == id))
        .or_else(|| settings.apis.iter().find(|p| !p.token.trim().is_empty()))
        .cloned()
}

pub fn is_api_expired(profile: &ApiProfile) -> bool {
    let Some(expires_at) = profile.expires_at.as_deref() else {
        return false;
    };
    match chrono::NaiveDate::parse_from_str(expires_at, "%Y-%m-%d") {
        Ok(date) => date < chrono::Local::now().date_naive(),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_without_expiry_is_available() {
        let p = ApiProfile {
            id: "a".to_string(),
            name: "A".to_string(),
            token: "token".to_string(),
            expires_at: None,
        };
        assert!(!is_api_expired(&p));
    }

    #[test]
    fn invalid_expiry_disables_api() {
        let p = ApiProfile {
            id: "a".to_string(),
            name: "A".to_string(),
            token: "token".to_string(),
            expires_at: Some("not-a-date".to_string()),
        };
        assert!(is_api_expired(&p));
    }
}
