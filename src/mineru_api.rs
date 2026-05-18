use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::time::sleep;

/// 与官方文档一致：https://mineru.net/apiManage/docs（精准解析 API 使用 mineru.net 域名）
const API_BASE: &str = "https://mineru.net";
const MAX_CONCURRENT: usize = 3;
const POLL_INTERVAL_SECS: u64 = 5;
const MAX_POLL_ATTEMPTS: u32 = 120;

#[derive(Debug, Deserialize)]
struct MineruEnvelope<T> {
    code: i32,
    msg: String,
    data: Option<T>,
}

#[derive(Debug, Serialize)]
struct BatchFileItem {
    name: String,
    /// 文档要求：字母数字下划线等；用前端任务 id 保证唯一且合法
    #[serde(rename = "data_id")]
    data_id: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "is_ocr")]
    is_ocr: Option<bool>,
}

#[derive(Debug, Serialize)]
struct BatchUrlsBody {
    files: Vec<BatchFileItem>,
    #[serde(rename = "model_version")]
    model_version: String,
    language: String,
    #[serde(rename = "enable_table")]
    enable_table: bool,
    #[serde(rename = "enable_formula")]
    enable_formula: bool,
}

#[derive(Debug, Deserialize)]
struct BatchUrlsData {
    #[serde(rename = "batch_id")]
    batch_id: String,
    #[serde(rename = "file_urls")]
    file_urls: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BatchExtractData {
    #[serde(rename = "batch_id")]
    #[allow(dead_code)]
    batch_id: Option<String>,
    #[serde(rename = "extract_result", default)]
    extract_result: Vec<ExtractResultItem>,
}

#[derive(Debug, Deserialize)]
struct ExtractResultItem {
    #[serde(rename = "file_name")]
    file_name: String,
    state: String,
    #[serde(default, rename = "full_zip_url")]
    full_zip_url: Option<String>,
    #[serde(default, rename = "err_msg")]
    err_msg: Option<String>,
    #[serde(default, rename = "data_id")]
    data_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionTask {
    pub id: String,
    #[serde(default)]
    pub api_profile_id: Option<String>,
    pub source_path: String,
    pub file_name: String,
    /// 与前端一致：同一源文件（含所有拆分任务）共用的输出子目录名
    pub bundle_folder: String,
    pub page_range: Option<(u32, u32)>,
    pub task_id: Option<String>,
    pub status: String,
    pub zip_url: Option<String>,
    pub error: Option<String>,
}

fn create_client(token: &str) -> reqwest::Client {
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", token)) {
        headers.insert(AUTHORIZATION, v);
    }
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client")
}

/// 申请批量上传 URL；返回 batch_id 与 PUT 上传地址列表（与 files 顺序一致）。
async fn request_batch_upload_urls(
    client: &reqwest::Client,
    items: Vec<BatchFileItem>,
) -> Result<(String, Vec<String>), String> {
    let body = BatchUrlsBody {
        files: items,
        model_version: "vlm".to_string(),
        language: "ch".to_string(),
        enable_table: true,
        enable_formula: true,
    };

    let response = client
        .post(&format!("{}/api/v4/file-urls/batch", API_BASE))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            format!(
                "获取上传 URL 失败: {}（请确认可访问 {}，且 Token 来自 MinerU 官网）",
                e, API_BASE
            )
        })?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("API 错误 {}: {}", status, text));
    }

    let env: MineruEnvelope<BatchUrlsData> =
        serde_json::from_str(&text).map_err(|e| format!("解析响应失败: {} — 正文: {}", e, text))?;

    if env.code != 0 {
        return Err(format!("获取上传 URL 失败 [{}]: {}", env.code, env.msg));
    }

    let data = env
        .data
        .ok_or_else(|| format!("响应缺少 data 字段: {}", text))?;

    if data.file_urls.is_empty() {
        return Err("响应中 file_urls 为空".to_string());
    }

    Ok((data.batch_id, data.file_urls))
}

/// 文档：上传时无须设置 Content-Type
async fn upload_file_put(url: &str, file_path: &str) -> Result<(), String> {
    let data = tokio::fs::read(file_path)
        .await
        .map_err(|e| format!("读取文件失败: {}", e))?;

    let client = reqwest::Client::new();
    let response = client
        .put(url)
        .body(data)
        .send()
        .await
        .map_err(|e| format!("上传文件失败: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("上传失败: {}", response.status()));
    }

    Ok(())
}

async fn fetch_batch_extract_results(
    client: &reqwest::Client,
    batch_id: &str,
) -> Result<Vec<ExtractResultItem>, String> {
    let response = client
        .get(&format!(
            "{}/api/v4/extract-results/batch/{}",
            API_BASE, batch_id
        ))
        .send()
        .await
        .map_err(|e| format!("查询批量结果失败: {}", e))?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("查询批量结果 HTTP {}: {}", status, text));
    }

    let env: MineruEnvelope<BatchExtractData> =
        serde_json::from_str(&text).map_err(|e| format!("解析批量结果失败: {} — {}", e, text))?;

    if env.code != 0 {
        return Err(format!("查询批量结果失败 [{}]: {}", env.code, env.msg));
    }

    Ok(env.data.map(|d| d.extract_result).unwrap_or_default())
}

fn map_mineru_state(state: &str) -> (&'static str, f32) {
    match state {
        // 与前端 `parsed` 一致：解析完成但仍需下载/整理，避免与最终 `done` 混淆
        "done" => ("parsed", 0.92),
        "failed" => ("failed", 0.0),
        "running" | "converting" => ("converting", 0.65),
        "pending" | "waiting-file" => ("pending", 0.4),
        _ => ("polling", 0.5),
    }
}

fn find_extract_item<'a>(
    items: &'a [ExtractResultItem],
    local_task_id: &str,
    file_name: &str,
) -> Option<&'a ExtractResultItem> {
    items
        .iter()
        .find(|it| it.data_id.as_deref() == Some(local_task_id))
        .or_else(|| items.iter().find(|it| it.file_name == file_name))
}

pub async fn upload_and_convert(
    token: String,
    files: Vec<ConversionTask>,
) -> Result<Vec<ConversionTask>, String> {
    let client = create_client(&token);
    let mut results = files;
    let mut pending_indices: Vec<usize> = (0..results.len()).collect();

    while !pending_indices.is_empty() {
        let batch: Vec<usize> = pending_indices
            .drain(..std::cmp::min(MAX_CONCURRENT, pending_indices.len()))
            .collect();

        let mut handles = Vec::new();

        for idx in batch {
            let task = results[idx].clone();
            let client = client.clone();

            let handle = tokio::spawn(async move {
                log::info!("上传: {}", task.file_name);

                let name_on_disk = Path::new(&task.source_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file.pdf")
                    .to_string();

                let item = BatchFileItem {
                    name: name_on_disk,
                    data_id: task.id.clone(),
                    is_ocr: Some(true),
                };

                let (batch_id, urls) = match request_batch_upload_urls(&client, vec![item]).await {
                    Ok(v) => v,
                    Err(e) => return (idx, Err(e)),
                };

                let put_url = urls.into_iter().next().unwrap();
                if let Err(e) = upload_file_put(&put_url, &task.source_path).await {
                    return (idx, Err(e));
                }

                log::info!("已上传，排队解析中: {}", task.file_name);

                // 文档：上传完成后由服务端自动提交解析；用 batch_id 轮询批量结果接口
                (idx, Ok(batch_id))
            });

            handles.push(handle);
        }

        for handle in handles {
            if let Ok((idx, result)) = handle.await {
                match result {
                    Ok(batch_id) => {
                        if let Some(t) = results.get_mut(idx) {
                            t.task_id = Some(batch_id);
                            t.status = "pending".to_string();
                        }
                    }
                    Err(e) => {
                        if let Some(t) = results.get_mut(idx) {
                            t.status = "failed".to_string();
                            t.error = Some(e);
                        }
                    }
                }
            }
        }
    }

    Ok(results)
}

pub async fn poll_tasks(
    token: String,
    tasks: Vec<ConversionTask>,
) -> Result<Vec<ConversionTask>, String> {
    let client = create_client(&token);
    let mut pending_batch_ids: Vec<String> =
        tasks.iter().filter_map(|t| t.task_id.clone()).collect();

    let mut completed: Vec<ConversionTask> = Vec::new();
    let mut attempts: HashMap<String, u32> = HashMap::new();

    for task in &tasks {
        attempts.insert(task.task_id.clone().unwrap_or_default(), 0);
    }

    while !pending_batch_ids.is_empty() {
        let mut new_pending: Vec<String> = Vec::new();

        for batch_id in pending_batch_ids {
            let task_match = tasks
                .iter()
                .find(|t| t.task_id.as_deref() == Some(batch_id.as_str()));

            let items = match fetch_batch_extract_results(&client, &batch_id).await {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("查询 batch {} 失败: {}", batch_id, e);
                    new_pending.push(batch_id);
                    continue;
                }
            };

            let Some(task) = task_match else {
                new_pending.push(batch_id);
                continue;
            };

            let Some(entry) = find_extract_item(&items, &task.id, &task.file_name) else {
                let count = attempts.entry(batch_id.clone()).or_insert(0);
                *count += 1;
                if *count < MAX_POLL_ATTEMPTS {
                    new_pending.push(batch_id);
                } else {
                    let mut failed_task = task.clone();
                    failed_task.status = "failed".to_string();
                    failed_task.error = Some("批量结果中未找到该任务或轮询超时".to_string());
                    completed.push(failed_task);
                }
                continue;
            };

            let (ui_status, progress) = map_mineru_state(entry.state.as_str());

            log::info!(
                "解析状态: {} {} {:.0}% {}",
                task.file_name,
                ui_status,
                progress * 100.0,
                entry.err_msg.clone().unwrap_or_else(|| entry.state.clone())
            );

            match entry.state.as_str() {
                "done" => {
                    let mut done_task = task.clone();
                    done_task.status = "parsed".to_string();
                    done_task.zip_url = entry.full_zip_url.clone();
                    completed.push(done_task);
                }
                "failed" => {
                    let mut failed_task = task.clone();
                    failed_task.status = "failed".to_string();
                    failed_task.error = entry
                        .err_msg
                        .clone()
                        .or_else(|| Some("解析失败".to_string()));
                    completed.push(failed_task);
                }
                _ => {
                    let count = attempts.entry(batch_id.clone()).or_insert(0);
                    *count += 1;
                    if *count < MAX_POLL_ATTEMPTS {
                        new_pending.push(batch_id);
                    } else {
                        let mut failed_task = task.clone();
                        failed_task.status = "failed".to_string();
                        failed_task.error = Some("解析等待超时".to_string());
                        completed.push(failed_task);
                    }
                }
            }
        }

        pending_batch_ids = new_pending;
        if !pending_batch_ids.is_empty() {
            sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
        }
    }

    // 与入参任务顺序一致，合并：轮询结果 + 上传失败未带 batch_id 的任务 + 异常遗漏
    let mut out: Vec<ConversionTask> = Vec::new();
    for t in &tasks {
        if let Some(c) = completed.iter().find(|c| c.id == t.id) {
            out.push(c.clone());
            continue;
        }
        if t.task_id.is_none() {
            out.push(t.clone());
            continue;
        }
        let mut tt = t.clone();
        tt.status = "failed".to_string();
        tt.error = Some(tt.error.unwrap_or_else(|| "轮询未返回最终状态".to_string()));
        out.push(tt);
    }

    Ok(out)
}
