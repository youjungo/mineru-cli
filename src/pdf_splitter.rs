use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use lopdf::Document;
use rayon::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitChunk {
    pub path: String,
    pub start_page: u32,
    pub end_page: u32,
    pub page_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitResult {
    pub original_path: String,
    pub original_name: String,
    pub chunks: Vec<SplitChunk>,
}

const MAX_PAGES_PER_CHUNK: u32 = 100;
const MIN_PAGES_PER_CHUNK: u32 = 1;
const MAX_PAGES_PER_CHUNK_LIMIT: u32 = 1000;

/// 并行拆分时分片任务上限，避免多路同时整本加载超大 PDF 撑爆内存
const MAX_SPLIT_WORKERS: usize = 8;

/// 日志中使用的短文件名（避免整段路径过长）
fn file_label_for_log(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}

fn calculate_chunks(page_count: u32, max_pages_per_chunk: u32) -> Vec<(u32, u32)> {
    let mut chunks: Vec<(u32, u32)> = Vec::new();
    let mut current = 1u32;

    while current <= page_count {
        let end = std::cmp::min(current + max_pages_per_chunk - 1, page_count);
        chunks.push((current, end));
        current = end + 1;
    }

    chunks
}

/// 生成单个分片文件（在 rayon 工作线程中调用）。
fn process_one_chunk(
    idx: usize,
    start: u32,
    end: u32,
    page_count: u32,
    source_path: &str,
    output_dir: &str,
    file_stem: &str,
    file_extension: &str,
    total_chunks: usize,
) -> Result<SplitChunk, String> {
    let chunk_wall = Instant::now();
    let mut to_delete: Vec<u32> = (1..=page_count).filter(|&p| p < start || p > end).collect();
    to_delete.sort_by(|a, b| b.cmp(a));

    log::info!(
        "[split_pdf] 分片 {}/{} 页范围 {}–{} 将删 {} 页 file={}",
        idx + 1,
        total_chunks,
        start,
        end,
        to_delete.len(),
        file_label_for_log(source_path)
    );

    let t = Instant::now();
    let mut chunk_doc = Document::load(source_path).map_err(|e| format!("加载 PDF 失败: {}", e))?;
    log::info!(
        "[split_pdf] 分片 {} Document::load {:?} file={}",
        idx + 1,
        t.elapsed(),
        file_label_for_log(source_path)
    );

    let t = Instant::now();
    if !to_delete.is_empty() {
        chunk_doc.delete_pages(&to_delete);
    }
    log::info!(
        "[split_pdf] 分片 {} delete_pages {:?} file={}",
        idx + 1,
        t.elapsed(),
        file_label_for_log(source_path)
    );

    let t = Instant::now();
    chunk_doc.prune_objects();
    log::info!(
        "[split_pdf] 分片 {} prune_objects {:?} file={}",
        idx + 1,
        t.elapsed(),
        file_label_for_log(source_path)
    );

    let t = Instant::now();
    chunk_doc.renumber_objects();
    log::info!(
        "[split_pdf] 分片 {} renumber_objects {:?} file={}",
        idx + 1,
        t.elapsed(),
        file_label_for_log(source_path)
    );

    let chunk_name = format!(
        "{}_part{}_{}-{}页.{}",
        file_stem,
        idx + 1,
        start,
        end,
        file_extension
    );
    let chunk_path = Path::new(output_dir).join(&chunk_name);
    let chunk_path_str = chunk_path.to_string_lossy().to_string();

    let t = Instant::now();
    chunk_doc
        .save(&chunk_path)
        .map_err(|e| format!("保存拆分文件失败: {}", e))?;
    log::info!(
        "[split_pdf] 分片 {} save {:?} file={}",
        idx + 1,
        t.elapsed(),
        file_label_for_log(source_path)
    );
    log::info!(
        "[split_pdf] 分片 {} 本段合计 {:?} file={}",
        idx + 1,
        chunk_wall.elapsed(),
        file_label_for_log(source_path)
    );

    Ok(SplitChunk {
        path: chunk_path_str,
        start_page: start,
        end_page: end,
        page_count: end - start + 1,
    })
}

/// 单次拆分最长等待时间；超时返回 `SPLIT_TIMEOUT`，由前端决定是否整份上传。
const SPLIT_TIMEOUT_SECS: u64 = 600;

/// 使用 lopdf 自带的 `delete_pages` + `prune_objects` + `renumber_objects` 做拆分：
/// 完整保留每页依赖的资源（字体、图片、XObject 等），避免仅复制 Page 字典导致的损坏小文件。
///
/// 多个分片使用 **rayon** 并行处理以利用多核；在阻塞线程中执行并设 10 分钟总超时。
pub async fn split_pdf(
    source_path: String,
    output_dir: String,
    max_pages_per_chunk: Option<u32>,
) -> Result<SplitResult, String> {
    let join = tokio::task::spawn_blocking(move || {
        split_pdf_blocking(source_path, output_dir, max_pages_per_chunk)
    });
    match tokio::time::timeout(std::time::Duration::from_secs(SPLIT_TIMEOUT_SECS), join).await {
        Err(_) => Err("SPLIT_TIMEOUT".to_string()),
        Ok(Ok(Ok(result))) => Ok(result),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(join_err)) => Err(format!("拆分任务异常: {}", join_err)),
    }
}

fn split_pdf_blocking(
    source_path: String,
    output_dir: String,
    max_pages_per_chunk: Option<u32>,
) -> Result<SplitResult, String> {
    let source = Path::new(&source_path);
    let file_stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("document")
        .to_string();
    let file_extension = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("pdf")
        .to_string();

    fs::create_dir_all(&output_dir).map_err(|e| format!("创建输出目录失败: {}", e))?;

    let split_t0 = Instant::now();
    log::info!(
        "[split_pdf] 开始 file={} output_dir={}",
        file_label_for_log(&source_path),
        output_dir
    );

    let page_count = {
        let t = Instant::now();
        let doc = Document::load(&source_path).map_err(|e| format!("加载 PDF 失败: {}", e))?;
        let n = doc.get_pages().len() as u32;
        log::info!(
            "[split_pdf] 初次加载并统计页数: {} 页，耗时 {:?} file={}",
            n,
            t.elapsed(),
            file_label_for_log(&source_path)
        );
        if n == 0 {
            return Err("PDF 文件为空".to_string());
        }
        n
    };

    let max_pages_per_chunk = max_pages_per_chunk
        .unwrap_or(MAX_PAGES_PER_CHUNK)
        .clamp(MIN_PAGES_PER_CHUNK, MAX_PAGES_PER_CHUNK_LIMIT);
    let chunks = calculate_chunks(page_count, max_pages_per_chunk);
    let total = chunks.len() as u32;
    let file_label = format!("{}.{}", file_stem, file_extension);

    let emit = |current: u32, message: String| {
        log::info!(
            "[split_pdf] {} {}/{} {}",
            file_label,
            current,
            total,
            message
        );
    };

    emit(
        0,
        format!(
            "共 {} 页，按每片最多 {} 页拆成 {} 个文件（并行处理）…",
            page_count, max_pages_per_chunk, total
        ),
    );

    let available = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let n_workers = chunks.len().min(available.max(1).min(MAX_SPLIT_WORKERS));
    log::info!(
        "[split_pdf] 并行工作线程数 {} / 分片数 {} file={}",
        n_workers,
        chunks.len(),
        file_label_for_log(&source_path)
    );

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(n_workers)
        .build()
        .map_err(|e| format!("创建拆分线程池失败: {}", e))?;

    let done = Arc::new(AtomicU32::new(0));
    let chunk_results: Vec<Result<SplitChunk, String>> = pool.install(|| {
        chunks
            .par_iter()
            .enumerate()
            .map(|(idx, &(start, end))| {
                let r = process_one_chunk(
                    idx,
                    start,
                    end,
                    page_count,
                    &source_path,
                    &output_dir,
                    &file_stem,
                    &file_extension,
                    chunks.len(),
                );
                if r.is_ok() {
                    let k = done.fetch_add(1, Ordering::SeqCst) + 1;
                    log::info!(
                        "[split_pdf] {} {}/{} 并行拆分完成（第 {}–{} 页）",
                        file_label,
                        k,
                        total,
                        start,
                        end
                    );
                }
                r
            })
            .collect()
    });

    let split_chunks: Vec<SplitChunk> = chunk_results.into_iter().collect::<Result<Vec<_>, _>>()?;

    log::info!(
        "[split_pdf] 全部完成 总分片={} 总耗时 {:?} file={}",
        split_chunks.len(),
        split_t0.elapsed(),
        file_label_for_log(&source_path)
    );

    Ok(SplitResult {
        original_path: source_path,
        original_name: format!("{}.{}", file_stem, file_extension),
        chunks: split_chunks,
    })
}
