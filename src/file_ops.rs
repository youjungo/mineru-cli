use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub file_type: String,
    pub page_count: Option<u32>,
    pub needs_split: bool,
    pub split_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileValidationResult {
    pub valid_files: Vec<FileInfo>,
    pub invalid_files: Vec<InvalidFileInfo>,
    pub files_needing_split: Vec<FileInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvalidFileInfo {
    pub path: String,
    pub name: String,
    pub reason: String,
}

const MAX_FILE_SIZE: u64 = 200 * 1024 * 1024; // 200MB
/// 本地拆分每片页数上限；整份上传单任务另受 600 页约束（见前端 `MAX_PDF_WHOLE_UPLOAD_*`）
const MAX_PDF_PAGES: u32 = 100;
const MIN_PDF_PAGES_LIMIT: u32 = 1;
const MAX_PDF_PAGES_LIMIT: u32 = 1000;

/// 扩展名统一小写，避免 `.PDF`、`.PNG` 等在部分系统上被误判为未知类型。
fn extension_lower(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
}

fn get_file_type(path: &str) -> String {
    match extension_lower(path).as_deref() {
        Some("pdf") => "pdf".to_string(),
        Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("bmp") | Some("webp") => {
            "image".to_string()
        }
        Some("doc") | Some("docx") => "word".to_string(),
        Some("ppt") | Some("pptx") => "ppt".to_string(),
        Some("html") | Some("htm") => "html".to_string(),
        _ => "unknown".to_string(),
    }
}

fn get_pdf_page_count(path: &str) -> Result<u32, String> {
    let doc = lopdf::Document::load(path).map_err(|e| e.to_string())?;
    Ok(doc.get_pages().len() as u32)
}

fn sanitize_max_pdf_pages(max_pdf_pages: Option<u32>) -> u32 {
    max_pdf_pages
        .unwrap_or(MAX_PDF_PAGES)
        .clamp(MIN_PDF_PAGES_LIMIT, MAX_PDF_PAGES_LIMIT)
}

pub async fn get_file_info(path: String, max_pdf_pages: Option<u32>) -> Result<FileInfo, String> {
    let path_obj = Path::new(&path);
    let metadata = fs::metadata(&path).map_err(|e| format!("无法读取文件: {}", e))?;
    let name = path_obj
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_type = get_file_type(&path);
    let mut page_count: Option<u32> = None;
    let mut needs_split = false;
    let mut split_reason: Option<String> = None;

    let max_pdf_pages = sanitize_max_pdf_pages(max_pdf_pages);

    if file_type == "pdf" {
        match get_pdf_page_count(&path) {
            Ok(pages) => {
                page_count = Some(pages);
                if pages > max_pdf_pages {
                    needs_split = true;
                    split_reason =
                        Some(format!("PDF 页数 {} 超过限制 {} 页", pages, max_pdf_pages));
                }
            }
            Err(_e) => {
                // 部分扫描件/线性化 PDF 等 lopdf 无法解析，仍允许加入队列整份上传；
                // 页数未知时用量预估按 1 页计（见前端 computePagesForTask）。
                page_count = None;
            }
        }
    }

    if metadata.len() > MAX_FILE_SIZE {
        needs_split = true;
        if file_type == "pdf" {
            split_reason = Some(format!(
                "文件大小 {}MB 超过限制 200MB",
                metadata.len() / (1024 * 1024)
            ));
        } else {
            return Err(format!(
                "文件大小 {}MB 超过 200MB 限制，非 PDF 文件不支持拆分",
                metadata.len() / (1024 * 1024)
            ));
        }
    }

    Ok(FileInfo {
        path,
        name,
        size: metadata.len(),
        file_type,
        page_count,
        needs_split,
        split_reason,
    })
}

pub async fn validate_files(
    paths: Vec<String>,
    max_pdf_pages: Option<u32>,
) -> Result<FileValidationResult, String> {
    let mut valid_files: Vec<FileInfo> = Vec::new();
    let mut invalid_files: Vec<InvalidFileInfo> = Vec::new();
    let mut files_needing_split: Vec<FileInfo> = Vec::new();

    for path in paths {
        match get_file_info(path.clone(), max_pdf_pages).await {
            Ok(info) => {
                if info.file_type == "unknown" {
                    invalid_files.push(InvalidFileInfo {
                        path,
                        name: info.name,
                        reason: "不支持的文件类型".to_string(),
                    });
                } else if info.needs_split && info.file_type != "pdf" {
                    invalid_files.push(InvalidFileInfo {
                        path,
                        name: info.name,
                        reason: info
                            .split_reason
                            .unwrap_or_else(|| "文件需要拆分".to_string()),
                    });
                } else if info.needs_split {
                    files_needing_split.push(info);
                } else {
                    valid_files.push(info);
                }
            }
            Err(e) => {
                let name = Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                invalid_files.push(InvalidFileInfo {
                    path,
                    name,
                    reason: e,
                });
            }
        }
    }

    Ok(FileValidationResult {
        valid_files,
        invalid_files,
        files_needing_split,
    })
}

/// 路径最后一段以 `.` 开头（如 `.git`、`.hidden`）则视为隐藏目录/文件，目录整棵子树不扫描。
fn basename_starts_with_dot(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|s| s.starts_with('.'))
}

fn collect_paths_recursive(
    dir: &Path,
    extensions: &[String],
    out: &mut Vec<String>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if basename_starts_with_dot(&path) {
                continue;
            }
            collect_paths_recursive(&path, extensions, out)?;
        } else if path.is_file() {
            if basename_starts_with_dot(&path) {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_lowercase());
            if let Some(ref e) = ext {
                if extensions.iter().any(|x| x.eq_ignore_ascii_case(e)) {
                    out.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
    Ok(())
}

/// 递归扫描目录及子目录，返回扩展名匹配的文件路径（用于批量添加）。
pub async fn collect_paths_from_directory(
    root: String,
    extensions: Vec<String>,
) -> Result<Vec<String>, String> {
    let root_path = Path::new(&root);
    if !root_path.is_dir() {
        return Err("不是有效目录".to_string());
    }
    if extensions.is_empty() {
        return Err("未指定要扫描的文件扩展名".to_string());
    }
    let mut out = Vec::new();
    collect_paths_recursive(root_path, &extensions, &mut out)
        .map_err(|e| format!("扫描目录失败: {}", e))?;
    out.sort();
    Ok(out)
}

/// 删除拆分产生的临时 PDF 等文件（仅删除存在的普通文件）。
pub fn delete_files(paths: Vec<String>) -> Result<u32, String> {
    let mut count = 0u32;
    for p in paths {
        let path = Path::new(&p);
        if path.is_file() {
            fs::remove_file(path).map_err(|e| format!("删除失败 {}: {}", p, e))?;
            count += 1;
        }
    }
    Ok(count)
}
