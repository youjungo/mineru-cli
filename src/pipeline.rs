use crate::file_ops::{
    collect_paths_from_directory, delete_files, validate_files, FileInfo, FileValidationResult,
};
use crate::mineru_api::{poll_tasks, upload_and_convert, ConversionTask};
use crate::output::{
    download_and_extract, fix_markdown_paths, organize_output, strip_markdown_images,
    OrganizeOptions,
};
use crate::pdf_splitter::{split_pdf, SplitResult};
use crate::settings::{is_api_expired, ApiProfile, AppSettings};
use crate::usage_stats::{get_usage_stats, record_usage_batch, UsageRecord};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

const DAILY_PAGE_LIMIT: u64 = 8_000;
const MAX_PDF_WHOLE_UPLOAD_BYTES: u64 = 200 * 1024 * 1024;
const MAX_PDF_WHOLE_UPLOAD_PAGES: u32 = 600;
const QUOTA_ERROR_MARKERS: &[&str] = &[
    "每日解析任务数量已达上限",
    "quota",
    "limit",
    "-60018",
    "额度",
    "上限",
];

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub inputs: Vec<String>,
    pub output_dir: Option<String>,
    pub use_source_dir: bool,
    pub token: Option<String>,
    pub api_id: Option<String>,
    pub types: Vec<String>,
    pub recursive: bool,
    pub pool_size: u32,
    pub split_pages: u32,
    pub delete_split_pdfs: bool,
    pub delete_originals: bool,
    pub balance_apis: bool,
    pub dry_run: bool,
    pub json: bool,
    pub is_ocr: bool,
    pub output_assets: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConvertSummary {
    pub valid_files: usize,
    pub invalid_files: usize,
    pub tasks_total: usize,
    pub tasks_done: usize,
    pub tasks_failed: usize,
    pub output_dir: Option<String>,
}

pub fn default_extensions(types: &[String]) -> Vec<String> {
    let all = if types.is_empty() {
        vec!["pdf", "image", "word", "ppt", "html"]
    } else {
        types.iter().map(String::as_str).collect()
    };
    let mut exts = Vec::new();
    for t in all {
        match t {
            "pdf" => exts.push("pdf"),
            "image" => exts.extend(["png", "jpg", "jpeg", "gif", "bmp", "webp"]),
            "word" => exts.extend(["doc", "docx"]),
            "ppt" => exts.extend(["ppt", "pptx"]),
            "html" => exts.extend(["html", "htm"]),
            ext => exts.push(ext),
        }
    }
    exts.into_iter().map(str::to_string).collect()
}

pub fn make_bundle_folder(file_index: usize, display_name: &str) -> String {
    let base = display_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(display_name);
    let safe: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\0'..='\u{1f}' => '_',
            c if c.is_whitespace() => ' ',
            c => c,
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(120)
        .collect();
    format!(
        "{}_{}",
        file_index,
        if safe.is_empty() { "document" } else { &safe }
    )
}

fn json_event<T: Serialize>(json: bool, event: &str, payload: &T) {
    if json {
        let value = serde_json::json!({ "event": event, "data": payload });
        println!("{}", value);
    }
}

fn info(json: bool, message: impl AsRef<str>) {
    if !json {
        println!("{}", message.as_ref());
    }
}

fn file_type_allowed(file_type: &str, types: &[String]) -> bool {
    types.is_empty() || types.iter().any(|t| t == file_type)
}

pub async fn collect_and_validate(
    options: &ConvertOptions,
) -> Result<FileValidationResult, String> {
    let extensions = default_extensions(&options.types);
    let mut paths = Vec::new();
    for input in &options.inputs {
        let p = Path::new(input);
        if p.is_dir() {
            let mut found = collect_paths_from_directory(input.clone(), extensions.clone()).await?;
            if !options.recursive {
                let root = p
                    .canonicalize()
                    .map_err(|e| format!("读取目录失败: {}", e))?;
                found.retain(|f| {
                    Path::new(f)
                        .parent()
                        .and_then(|parent| parent.canonicalize().ok())
                        .is_some_and(|parent| parent == root)
                });
            }
            paths.extend(found);
        } else {
            paths.push(input.clone());
        }
    }
    paths.sort();
    paths.dedup();

    let mut result = validate_files(paths, Some(options.split_pages)).await?;
    result
        .valid_files
        .retain(|f| file_type_allowed(&f.file_type, &options.types));
    result
        .files_needing_split
        .retain(|f| file_type_allowed(&f.file_type, &options.types));
    Ok(result)
}

fn resolve_output_dir(
    task: &ConversionTask,
    files: &[FileInfo],
    output_dir: &str,
    use_source_dir: bool,
) -> String {
    if !use_source_dir {
        return output_dir.to_string();
    }
    let raw = task.id.split('-').next().unwrap_or_default();
    let idx = raw.parse::<usize>().unwrap_or(usize::MAX);
    files
        .get(idx)
        .and_then(|f| Path::new(&f.path).parent())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| output_dir.to_string())
}

fn compute_pages_for_task(task: &ConversionTask, files: &[FileInfo]) -> UsageRecord {
    let raw = task.id.split('-').next().unwrap_or_default();
    let idx = raw.parse::<usize>().unwrap_or(usize::MAX);
    let file = files.get(idx);
    let file_type = file
        .map(|f| f.file_type.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let pages = if let Some((a, b)) = task.page_range {
        b.saturating_sub(a).saturating_add(1)
    } else if file_type == "pdf" {
        file.and_then(|f| f.page_count).unwrap_or(1)
    } else {
        1
    };
    UsageRecord { pages, file_type }
}

fn pages_for_file(file: &FileInfo) -> u32 {
    if file.file_type == "pdf" {
        file.page_count.unwrap_or(1)
    } else {
        1
    }
}

async fn split_files(
    files: &[FileInfo],
    output_dir: &str,
    split_pages: u32,
    json: bool,
) -> Vec<SplitResult> {
    let mut results = Vec::new();
    for file in files
        .iter()
        .filter(|f| f.needs_split && f.file_type == "pdf")
    {
        info(json, format!("拆分: {}", file.name));
        match split_pdf(file.path.clone(), output_dir.to_string(), Some(split_pages)).await {
            Ok(result) => {
                info(
                    json,
                    format!("拆分完成: {} -> {} 个分片", file.name, result.chunks.len()),
                );
                json_event(json, "split_done", &result);
                results.push(result);
            }
            Err(e)
                if e.contains("SPLIT_TIMEOUT")
                    && file.size <= MAX_PDF_WHOLE_UPLOAD_BYTES
                    && file.page_count.unwrap_or(0) <= MAX_PDF_WHOLE_UPLOAD_PAGES =>
            {
                info(json, format!("拆分超时，改为整份上传: {}", file.name));
            }
            Err(e) => {
                info(json, format!("拆分失败，跳过该文件: {}: {}", file.name, e));
            }
        }
    }
    results
}

fn build_tasks(files: &[FileInfo], split_results: &[SplitResult]) -> Vec<ConversionTask> {
    let mut tasks = Vec::new();
    for (idx, file) in files.iter().enumerate() {
        let bundle_folder = make_bundle_folder(idx, &file.name);
        if file.needs_split {
            if let Some(sr) = split_results.iter().find(|r| r.original_path == file.path) {
                for (chunk_idx, chunk) in sr.chunks.iter().enumerate() {
                    tasks.push(ConversionTask {
                        id: format!("{}-{}", idx, chunk_idx),
                        api_profile_id: None,
                        source_path: chunk.path.clone(),
                        file_name: file.name.clone(),
                        bundle_folder: bundle_folder.clone(),
                        page_range: Some((chunk.start_page, chunk.end_page)),
                        task_id: None,
                        status: "pending".to_string(),
                        zip_url: None,
                        error: None,
                    });
                }
            }
        } else {
            tasks.push(ConversionTask {
                id: idx.to_string(),
                api_profile_id: None,
                source_path: file.path.clone(),
                file_name: file.name.clone(),
                bundle_folder,
                page_range: None,
                task_id: None,
                status: "pending".to_string(),
                zip_url: None,
                error: None,
            });
        }
    }
    tasks
}

fn estimate_task_count(files: &[FileInfo], split_pages: u32) -> usize {
    files
        .iter()
        .map(|file| {
            if file.needs_split && file.file_type == "pdf" {
                file.page_count
                    .map(|pages| pages.div_ceil(split_pages.max(1)) as usize)
                    .unwrap_or(1)
                    .max(1)
            } else {
                1
            }
        })
        .sum()
}

async fn choose_profiles(
    settings: &AppSettings,
    token: &Option<String>,
    api_id: &Option<String>,
) -> Result<(Vec<ApiProfile>, bool), String> {
    if let Some(token) = token {
        let token = token.trim();
        if token.is_empty() {
            return Err("Token 不能为空".to_string());
        }
        return Ok((
            vec![ApiProfile {
                id: "cli-token".to_string(),
                name: "CLI Token".to_string(),
                token: token.to_string(),
                expires_at: None,
            }],
            true,
        ));
    }
    if let Some(id) = api_id {
        let p = settings
            .apis
            .iter()
            .find(|p| &p.id == id)
            .cloned()
            .ok_or_else(|| format!("未找到 API 配置: {}", id))?;
        if p.token.trim().is_empty() {
            return Err(format!("API 配置缺少 Token: {}", id));
        }
        if is_api_expired(&p) {
            return Err(format!("API 配置已过期: {}", id));
        }
        return Ok((vec![p], true));
    }
    let mut out: Vec<ApiProfile> = settings
        .apis
        .iter()
        .filter(|p| !p.token.trim().is_empty() && !is_api_expired(p))
        .cloned()
        .collect();
    if out.is_empty() {
        return Err(
            "没有可用 API：请检查 Token 是否为空、是否已过期，或使用 --api 强制指定".to_string(),
        );
    }
    rotate_profiles_by_time(&mut out);
    Ok((out, false))
}

fn rotate_profiles_by_time(profiles: &mut [ApiProfile]) {
    if profiles.len() < 2 {
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    profiles.rotate_left(nanos % profiles.len());
}

async fn assign_apis(
    tasks: Vec<ConversionTask>,
    files: &[FileInfo],
    profiles: &[ApiProfile],
    balance: bool,
    allow_over_quota: bool,
) -> Vec<ConversionTask> {
    let mut remaining = HashMap::new();
    for p in profiles {
        let used = get_usage_stats(p.token.clone())
            .await
            .map(|s| s.total_pages)
            .unwrap_or(0);
        remaining.insert(p.id.clone(), DAILY_PAGE_LIMIT.saturating_sub(used));
    }
    let mut counts: HashMap<String, usize> = profiles.iter().map(|p| (p.id.clone(), 0)).collect();
    let mut assigned = Vec::new();
    for mut task in tasks {
        let pages = compute_pages_for_task(&task, files).pages as u64;
        let mut pool: Vec<&ApiProfile> = profiles
            .iter()
            .filter(|p| remaining.get(&p.id).copied().unwrap_or(0) >= pages)
            .collect();
        if pool.is_empty() {
            if allow_over_quota {
                pool = profiles.iter().collect();
            } else {
                task.status = "failed".to_string();
                task.error = Some("所有 API 今日剩余额度不足".to_string());
                assigned.push(task);
                continue;
            }
        }
        let chosen = if balance && pool.len() > 1 {
            pool.into_iter()
                .min_by_key(|p| counts.get(&p.id).copied().unwrap_or(0))
                .unwrap()
        } else {
            pool[0]
        };
        *counts.entry(chosen.id.clone()).or_default() += 1;
        let rem = remaining.entry(chosen.id.clone()).or_default();
        *rem = rem.saturating_sub(pages);
        task.api_profile_id = Some(chosen.id.clone());
        assigned.push(task);
    }
    assigned
}

async fn download_and_organize(
    mut tasks: Vec<ConversionTask>,
    files: &[FileInfo],
    profiles: &[ApiProfile],
    output_dir: &str,
    use_source_dir: bool,
    json: bool,
    output_assets: bool,
) -> Vec<ConversionTask> {
    let profile_tokens: HashMap<String, String> = profiles
        .iter()
        .map(|p| (p.id.clone(), p.token.clone()))
        .collect();
    let mut usage: HashMap<String, Vec<UsageRecord>> = HashMap::new();

    for idx in 0..tasks.len() {
        if tasks[idx].status != "parsed" || tasks[idx].zip_url.is_none() {
            continue;
        }
        let task = tasks[idx].clone();
        let task_output_dir = resolve_output_dir(&task, files, output_dir, use_source_dir);
        info(json, format!("下载: {}", task.file_name));
        match download_and_extract(
            task.zip_url.clone().unwrap(),
            task_output_dir.clone(),
            task.bundle_folder.clone(),
            task.id.clone(),
        )
        .await
        {
            Ok(download) if download.success && download.extracted_dir.is_some() => {
                let bundle_dir = Path::new(&task_output_dir).join(&task.bundle_folder);
                let options = OrganizeOptions {
                    extract_dir: download.extracted_dir.unwrap(),
                    bundle_dir: bundle_dir.to_string_lossy().to_string(),
                    original_name: task.file_name.clone(),
                    has_page_range: task.page_range.is_some(),
                    page_start: task.page_range.map(|r| r.0),
                    page_end: task.page_range.map(|r| r.1),
                    image_name_prefix: task.page_range.map(|_| task.id.clone()),
                    copy_images: output_assets,
                };
                match organize_output(options).await {
                    Ok(result) if result.success => {
                        let path_result = if output_assets {
                            fix_markdown_paths(result.markdown_files.clone(), result.images_dir)
                                .await
                        } else {
                            strip_markdown_images(result.markdown_files.clone()).await
                        };
                        if let Err(e) = path_result {
                            tasks[idx].status = "failed".to_string();
                            tasks[idx].error = Some(e);
                            continue;
                        }
                        tasks[idx].status = "done".to_string();
                        tasks[idx].error = None;
                        if let Some(profile_id) = &task.api_profile_id {
                            if let Some(token) = profile_tokens.get(profile_id) {
                                usage
                                    .entry(token.clone())
                                    .or_default()
                                    .push(compute_pages_for_task(&task, files));
                            }
                        }
                        info(json, format!("完成: {}", task.file_name));
                    }
                    Ok(result) => {
                        tasks[idx].status = "failed".to_string();
                        tasks[idx].error =
                            result.error.or_else(|| Some("整理输出失败".to_string()));
                    }
                    Err(e) => {
                        tasks[idx].status = "failed".to_string();
                        tasks[idx].error = Some(e);
                    }
                }
            }
            Ok(download) => {
                tasks[idx].status = "failed".to_string();
                tasks[idx].error = download.error.or_else(|| Some("下载失败".to_string()));
            }
            Err(e) => {
                tasks[idx].status = "failed".to_string();
                tasks[idx].error = Some(e);
            }
        }
    }

    for (token, records) in usage {
        let _ = record_usage_batch(token, records).await;
    }
    tasks
}

fn is_quota_error(task: &ConversionTask) -> bool {
    let Some(error) = task.error.as_deref() else {
        return false;
    };
    let lower = error.to_lowercase();
    QUOTA_ERROR_MARKERS
        .iter()
        .any(|marker| lower.contains(&marker.to_lowercase()))
}

async fn submit_with_quota_retry(
    first_profile: &ApiProfile,
    profiles: &[ApiProfile],
    tasks: Vec<ConversionTask>,
    is_ocr: bool,
    allow_over_quota: bool,
) -> Result<Vec<ConversionTask>, String> {
    let mut remaining_tasks = tasks;
    let mut results = Vec::new();
    let mut tried = HashSet::new();
    let mut ordered = Vec::new();
    ordered.push(first_profile.clone());
    ordered.extend(
        profiles
            .iter()
            .filter(|p| p.id != first_profile.id)
            .cloned(),
    );

    for profile in ordered {
        if remaining_tasks.is_empty() {
            break;
        }
        if !allow_over_quota {
            let used = get_usage_stats(profile.token.clone())
                .await
                .map(|s| s.total_pages)
                .unwrap_or(0);
            if used >= DAILY_PAGE_LIMIT {
                continue;
            }
        }
        tried.insert(profile.id.clone());
        let profile_tasks: Vec<ConversionTask> = remaining_tasks
            .drain(..)
            .map(|mut t| {
                t.api_profile_id = Some(profile.id.clone());
                t.status = "pending".to_string();
                t.error = None;
                t.task_id = None;
                t.zip_url = None;
                t
            })
            .collect();
        let uploaded = upload_and_convert(profile.token.clone(), profile_tasks, is_ocr).await?;
        let polled = poll_tasks(profile.token.clone(), uploaded).await?;
        for task in polled {
            if task.status == "failed" && is_quota_error(&task) && !allow_over_quota {
                remaining_tasks.push(task);
            } else {
                results.push(task);
            }
        }
    }

    for mut task in remaining_tasks {
        task.status = "failed".to_string();
        task.error = Some(format!(
            "所有可用 API 均已超额或重试失败（已尝试 {} 个 API）",
            tried.len()
        ));
        results.push(task);
    }
    Ok(results)
}

fn cleanup(
    files: &[FileInfo],
    split_results: &[SplitResult],
    tasks: &[ConversionTask],
    delete_splits: bool,
    delete_originals: bool,
    json: bool,
) {
    let mut complete_bundles = HashSet::new();
    for (idx, file) in files.iter().enumerate() {
        let bundle = make_bundle_folder(idx, &file.name);
        let related: Vec<_> = tasks.iter().filter(|t| t.bundle_folder == bundle).collect();
        if !related.is_empty() && related.iter().all(|t| t.status == "done") {
            complete_bundles.insert((file.path.clone(), bundle));
        }
    }

    if delete_splits {
        let mut paths = Vec::new();
        for sr in split_results {
            if complete_bundles.iter().any(|(p, _)| p == &sr.original_path) {
                paths.extend(sr.chunks.iter().map(|c| c.path.clone()));
            }
        }
        if !paths.is_empty() {
            match delete_files(paths) {
                Ok(n) => info(json, format!("已删除 {} 个临时拆分 PDF", n)),
                Err(e) => info(json, format!("删除临时拆分 PDF 失败: {}", e)),
            }
        }
    }

    if delete_originals {
        let paths: Vec<String> = complete_bundles.into_iter().map(|(p, _)| p).collect();
        if !paths.is_empty() {
            match delete_files(paths) {
                Ok(n) => info(json, format!("已删除 {} 个源文件", n)),
                Err(e) => info(json, format!("删除源文件失败: {}", e)),
            }
        }
    }
}

pub async fn run_convert(
    settings: &AppSettings,
    options: ConvertOptions,
) -> Result<ConvertSummary, String> {
    if options.inputs.is_empty() {
        return Err("请至少提供一个输入文件或目录".to_string());
    }

    let output_dir = options
        .output_dir
        .clone()
        .or_else(|| settings.output_dir.clone())
        .unwrap_or_else(|| ".".to_string());
    if !options.use_source_dir {
        std::fs::create_dir_all(&output_dir)
            .map_err(|e| format!("创建输出目录失败 {}: {}", output_dir, e))?;
    }

    let validation = collect_and_validate(&options).await?;
    let invalid_count = validation.invalid_files.len();
    let mut files = validation.valid_files;
    files.extend(validation.files_needing_split);

    if let Some(over_limit) = files
        .iter()
        .find(|f| f.file_type == "pdf" && f.page_count.unwrap_or(0) > DAILY_PAGE_LIMIT as u32)
    {
        return Err(format!(
            "PDF 超过每 API 每日上限 {} 页：{}（{} 页），已停止",
            DAILY_PAGE_LIMIT,
            over_limit.name,
            over_limit.page_count.unwrap_or(0)
        ));
    }

    if files.is_empty() {
        return Ok(ConvertSummary {
            valid_files: 0,
            invalid_files: invalid_count,
            tasks_total: 0,
            tasks_done: 0,
            tasks_failed: 0,
            output_dir: Some(output_dir),
        });
    }

    info(
        options.json,
        format!(
            "找到 {} 个有效文件，{} 个无效文件",
            files.len(),
            invalid_count
        ),
    );
    json_event(options.json, "validated", &files);

    if options.dry_run {
        let task_count = estimate_task_count(&files, options.split_pages);
        info(
            options.json,
            format!("Dry run: 将创建约 {} 个转换任务", task_count),
        );
        return Ok(ConvertSummary {
            valid_files: files.len(),
            invalid_files: invalid_count,
            tasks_total: task_count,
            tasks_done: 0,
            tasks_failed: 0,
            output_dir: Some(output_dir),
        });
    }

    let split_results = split_files(&files, &output_dir, options.split_pages, options.json).await;
    let tasks = build_tasks(&files, &split_results);

    let (profiles, allow_over_quota) =
        choose_profiles(settings, &options.token, &options.api_id).await?;
    if !allow_over_quota {
        let total_pages: u64 = files.iter().map(|f| pages_for_file(f) as u64).sum();
        let mut available_pages = 0u64;
        for profile in &profiles {
            let used = get_usage_stats(profile.token.clone())
                .await
                .map(|s| s.total_pages)
                .unwrap_or(0);
            available_pages += DAILY_PAGE_LIMIT.saturating_sub(used);
        }
        if available_pages == 0 {
            return Err(
                "所有 API 今日额度都已用尽；可使用 --api <id> 强制指定某个 API".to_string(),
            );
        }
        if total_pages > available_pages {
            info(
                options.json,
                format!(
                    "本次预计 {} 页，自动密钥池剩余额度 {} 页，部分任务可能无法分配",
                    total_pages, available_pages
                ),
            );
        }
    }
    let assigned = assign_apis(
        tasks,
        &files,
        &profiles,
        options.balance_apis,
        allow_over_quota,
    )
    .await;
    let pool_size = options.pool_size.max(1) as usize;
    let mut final_tasks: Vec<ConversionTask> = assigned
        .iter()
        .filter(|t| t.status == "failed")
        .cloned()
        .collect();

    let mut by_original: HashMap<usize, Vec<ConversionTask>> = HashMap::new();
    for task in assigned {
        if task.status == "failed" {
            continue;
        }
        let idx = task
            .id
            .split('-')
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        by_original.entry(idx).or_default().push(task);
    }
    let mut keys: Vec<_> = by_original.keys().copied().collect();
    keys.sort_unstable();

    for chunk in keys.chunks(pool_size) {
        let batch: Vec<ConversionTask> = chunk
            .iter()
            .flat_map(|k| by_original.remove(k).unwrap_or_default())
            .collect();
        for profile in &profiles {
            let profile_tasks: Vec<ConversionTask> = batch
                .iter()
                .filter(|t| t.api_profile_id.as_deref() == Some(profile.id.as_str()))
                .cloned()
                .collect();
            if profile_tasks.is_empty() {
                continue;
            }
            info(
                options.json,
                format!("{}: 提交 {} 个任务", profile.name, profile_tasks.len()),
            );
            let mut polled = submit_with_quota_retry(
                profile,
                &profiles,
                profile_tasks,
                options.is_ocr,
                allow_over_quota,
            )
            .await?;
            final_tasks.append(&mut polled);
        }
    }

    let final_tasks = download_and_organize(
        final_tasks,
        &files,
        &profiles,
        &output_dir,
        options.use_source_dir,
        options.json,
        options.output_assets,
    )
    .await;

    cleanup(
        &files,
        &split_results,
        &final_tasks,
        options.delete_split_pdfs,
        options.delete_originals,
        options.json,
    );

    let done = final_tasks.iter().filter(|t| t.status == "done").count();
    let failed = final_tasks.iter().filter(|t| t.status == "failed").count();
    for task in final_tasks.iter().filter(|t| t.status == "failed") {
        info(
            options.json,
            format!(
                "失败: {}: {}",
                task.file_name,
                task.error.clone().unwrap_or_default()
            ),
        );
    }

    Ok(ConvertSummary {
        valid_files: files.len(),
        invalid_files: invalid_count,
        tasks_total: final_tasks.len(),
        tasks_done: done,
        tasks_failed: failed,
        output_dir: Some(output_dir),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, file_type: &str, page_count: Option<u32>, needs_split: bool) -> FileInfo {
        FileInfo {
            path: format!("/tmp/{}", name),
            name: name.to_string(),
            size: 1,
            file_type: file_type.to_string(),
            page_count,
            needs_split,
            split_reason: None,
        }
    }

    #[test]
    fn bundle_folder_is_stable_and_path_safe() {
        assert_eq!(make_bundle_folder(3, "a/b:c?.pdf"), "3_a_b_c_");
        assert_eq!(make_bundle_folder(0, "report.pdf"), "0_report");
    }

    #[test]
    fn default_extensions_expand_groups() {
        let exts = default_extensions(&["pdf".to_string(), "image".to_string()]);
        assert!(exts.contains(&"pdf".to_string()));
        assert!(exts.contains(&"png".to_string()));
        assert!(exts.contains(&"webp".to_string()));
    }

    #[test]
    fn dry_run_task_count_estimates_split_pdf_chunks() {
        let files = vec![
            file("book.pdf", "pdf", Some(250), true),
            file("slide.pptx", "ppt", None, false),
        ];
        assert_eq!(estimate_task_count(&files, 100), 4);
    }
}
