//! 按 Token（哈希）统计每日解析页数，数据存于本地 JSON。
//! 配额数字以 mineru.net 官方说明为准，此处仅作参考展示。

use chrono::Local;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    /// 计入统计的页数（PDF 为实际页段；其他格式通常为 1）
    pub pages: u32,
    pub file_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DayStats {
    pages: u64,
    tasks: u64,
    #[serde(default)]
    by_type: HashMap<String, u64>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct UsageFile {
    /// token_sha256 -> YYYY-MM-DD -> 统计
    #[serde(default)]
    by_token: HashMap<String, HashMap<String, DayStats>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStatsResponse {
    pub date: String,
    pub total_pages: u64,
    pub total_tasks: u64,
    pub by_type: HashMap<String, u64>,
    /// 仅用于在界面区分不同 Token，不可逆推原文
    pub token_fingerprint: String,
}

fn usage_path() -> PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("mineru-cli");
    fs::create_dir_all(&p).ok();
    p.push("usage_stats.json");
    p
}

fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.trim().as_bytes());
    format!("{:x}", h.finalize())
}

fn fingerprint(full_hash: &str) -> String {
    full_hash.chars().take(8).collect()
}

fn load_usage() -> UsageFile {
    let path = usage_path();
    if path.exists() {
        if let Ok(s) = fs::read_to_string(&path) {
            if let Ok(u) = serde_json::from_str(&s) {
                return u;
            }
        }
    }
    UsageFile::default()
}

fn save_usage(u: &UsageFile) -> Result<(), String> {
    let path = usage_path();
    let json = serde_json::to_string_pretty(u).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| format!("写入用量统计失败: {}", e))
}

/// 当前 Token 今日用量（无 Token 时返回全 0）
pub async fn get_usage_stats(token: String) -> Result<UsageStatsResponse, String> {
    let token = token.trim();
    if token.is_empty() {
        let date = Local::now().date_naive().to_string();
        return Ok(UsageStatsResponse {
            date,
            total_pages: 0,
            total_tasks: 0,
            by_type: HashMap::new(),
            token_fingerprint: "—".to_string(),
        });
    }

    let key = hash_token(token);
    let fp = fingerprint(&key);
    let date = Local::now().date_naive().to_string();
    let u = load_usage();
    let empty = DayStats::default();
    let day = u
        .by_token
        .get(&key)
        .and_then(|m| m.get(&date))
        .unwrap_or(&empty);

    Ok(UsageStatsResponse {
        date,
        total_pages: day.pages,
        total_tasks: day.tasks,
        by_type: day.by_type.clone(),
        token_fingerprint: fp,
    })
}

/// 成功完成解析与下载整理后，累加页数（按 Token 分账）
pub async fn record_usage_batch(token: String, records: Vec<UsageRecord>) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() || records.is_empty() {
        return Ok(());
    }

    let key = hash_token(token);
    let date = Local::now().date_naive().to_string();

    let mut u = load_usage();
    let by_date = u.by_token.entry(key).or_default();
    let day = by_date.entry(date).or_insert_with(DayStats::default);

    for r in records {
        let pages = r.pages as u64;
        day.pages = day.pages.saturating_add(pages);
        day.tasks = day.tasks.saturating_add(1);
        *day.by_type.entry(r.file_type.clone()).or_insert(0) += pages;
    }

    save_usage(&u)
}
