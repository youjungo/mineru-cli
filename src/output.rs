use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use zip::ZipArchive;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    pub task_id: String,
    pub success: bool,
    pub extracted_dir: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizeOptions {
    /// 当前任务解压目录：`{bundle_dir}/.extract/{task_id}`
    pub extract_dir: String,
    /// 该源文件最终输出根目录（其下为多个 .md + `images/`）
    pub bundle_dir: String,
    pub original_name: String,
    pub has_page_range: bool,
    pub page_start: Option<u32>,
    pub page_end: Option<u32>,
    /// 拆分时用于图片文件名去重，如 `0_1`
    pub image_name_prefix: Option<String>,
    #[serde(default)]
    pub copy_images: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizeResult {
    pub success: bool,
    pub markdown_files: Vec<String>,
    pub images_dir: String,
    pub error: Option<String>,
}

fn safe_bundle_dir(base: &Path, segment: &str) -> Result<PathBuf, String> {
    if segment.is_empty() {
        return Err("bundle 目录名不能为空".to_string());
    }
    if segment.contains("..") {
        return Err("bundle 目录名不能包含 '..'".to_string());
    }
    for c in segment.chars() {
        if matches!(c, '/' | '\\') {
            return Err("bundle 目录名不能包含路径分隔符".to_string());
        }
    }
    Ok(base.join(segment))
}

pub async fn download_and_extract(
    zip_url: String,
    output_dir: String,
    bundle_folder: String,
    task_id: String,
) -> Result<DownloadResult, String> {
    let out_base = Path::new(&output_dir);
    let bundle_dir = safe_bundle_dir(out_base, &bundle_folder)?;
    fs::create_dir_all(&bundle_dir).map_err(|e| format!("创建输出目录失败: {}", e))?;

    let extract_dir = bundle_dir.join(".extract").join(&task_id);
    fs::create_dir_all(&extract_dir).map_err(|e| format!("创建解压目录失败: {}", e))?;

    let response = reqwest::get(&zip_url)
        .await
        .map_err(|e| format!("下载失败: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("下载失败: HTTP {}", response.status()));
    }

    let temp_zip_path = bundle_dir.join(format!("temp_{}.zip", task_id));

    let mut file = File::create(&temp_zip_path).map_err(|e| format!("创建临时文件失败: {}", e))?;

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("读取响应失败: {}", e))?;

    file.write_all(&bytes)
        .map_err(|e| format!("写入文件失败: {}", e))?;

    drop(file);

    let file = File::open(&temp_zip_path).map_err(|e| format!("打开压缩文件失败: {}", e))?;
    let mut archive =
        ZipArchive::new(BufReader::new(file)).map_err(|e| format!("解析 Zip 失败: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("读取 Zip 条目失败: {}", e))?;

        let outpath = match file.enclosed_name() {
            Some(path) => extract_dir.join(path),
            None => continue,
        };

        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath).ok();
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent).ok();
            }
            let mut outfile =
                File::create(&outpath).map_err(|e| format!("创建输出文件失败: {}", e))?;
            io::copy(&mut file, &mut outfile).map_err(|e| format!("复制文件内容失败: {}", e))?;
        }
    }

    fs::remove_file(&temp_zip_path).ok();

    Ok(DownloadResult {
        task_id,
        success: true,
        extracted_dir: Some(extract_dir.to_string_lossy().to_string()),
        error: None,
    })
}

pub async fn organize_output(options: OrganizeOptions) -> Result<OrganizeResult, String> {
    let extract_root = Path::new(&options.extract_dir);
    let bundle_dir = Path::new(&options.bundle_dir);
    fs::create_dir_all(bundle_dir).map_err(|e| format!("创建 bundle 目录失败: {}", e))?;

    let display_base = Path::new(&options.original_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(options.original_name.as_str());

    let images_dir = bundle_dir.join("images");
    if options.copy_images {
        fs::create_dir_all(&images_dir).map_err(|e| format!("创建图片目录失败: {}", e))?;
    }

    let mut markdown_files: Vec<String> = Vec::new();
    let prefix = options
        .image_name_prefix
        .as_deref()
        .map(|s| s.replace(['/', '\\', '.'], "_"))
        .filter(|s| !s.is_empty());

    fn copy_image_dedup(src: &Path, images_dir: &Path, prefix: Option<&str>) -> io::Result<()> {
        let img_name = src
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "无文件名"))?;
        let mut dest = images_dir.join(img_name);
        if dest.exists() {
            if let Some(p) = prefix {
                dest = images_dir.join(format!("{}_{}", p, img_name.to_string_lossy()));
            } else {
                dest = images_dir.join(format!("dup_{}", img_name.to_string_lossy()));
            }
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::copy(src, &dest)?;
        Ok(())
    }

    fn find_markdown_and_images(
        dir: &Path,
        images_dir: &Path,
        md_target_dir: &Path,
        original_name: &str,
        has_page_range: bool,
        page_start: Option<u32>,
        page_end: Option<u32>,
        prefix: Option<&str>,
        md_files: &mut Vec<String>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                find_markdown_and_images(
                    &path,
                    images_dir,
                    md_target_dir,
                    original_name,
                    has_page_range,
                    page_start,
                    page_end,
                    prefix,
                    md_files,
                )?;
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                match ext.to_lowercase().as_str() {
                    "md" => {
                        let file_name = if has_page_range {
                            if let (Some(start), Some(end)) = (page_start, page_end) {
                                format!("{}_{}-{}页.md", original_name, start, end)
                            } else {
                                format!("{}.md", original_name)
                            }
                        } else {
                            format!("{}.md", original_name)
                        };

                        let target_path = md_target_dir.join(&file_name);
                        if path != target_path {
                            if let Some(parent) = target_path.parent() {
                                fs::create_dir_all(parent).ok();
                            }
                            fs::copy(&path, &target_path)?;
                        }
                        md_files.push(target_path.to_string_lossy().to_string());
                    }
                    "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" => {
                        if images_dir.exists() {
                            copy_image_dedup(&path, images_dir, prefix)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    find_markdown_and_images(
        extract_root,
        &images_dir,
        bundle_dir,
        display_base,
        options.has_page_range,
        options.page_start,
        options.page_end,
        prefix.as_deref(),
        &mut markdown_files,
    )
    .map_err(|e| e.to_string())?;

    fs::remove_dir_all(extract_root).ok();

    Ok(OrganizeResult {
        success: true,
        markdown_files,
        images_dir: images_dir.to_string_lossy().to_string(),
        error: None,
    })
}

fn basename_from_src(src: &str) -> Option<String> {
    let t = src.trim();
    let t = t.trim_matches(|c| c == '<' || c == '>');
    if t.is_empty() {
        return None;
    }
    Path::new(t)
        .file_name()
        .and_then(|n| n.to_str())
        .map(String::from)
}

/// 自 `from_dir` 到 `target` 的相对路径（POSIX 风格，供 Markdown 使用）
fn relative_path_str(from_dir: &Path, target: &Path) -> Option<String> {
    let from_c: Vec<_> = from_dir.components().collect();
    let to_c: Vec<_> = target.components().collect();
    let mut i = 0usize;
    while i < from_c.len() && i < to_c.len() && from_c[i] == to_c[i] {
        i += 1;
    }
    let mut out = PathBuf::new();
    for _ in i..from_c.len() {
        out.push("..");
    }
    for c in to_c.iter().skip(i) {
        match c {
            Component::Normal(os) => out.push(os),
            Component::ParentDir => out.push(".."),
            _ => {}
        }
    }
    if out.as_os_str().is_empty() {
        return None;
    }
    Some(out.to_string_lossy().replace('\\', "/"))
}

fn patch_html_img_tags(
    text: &str,
    re: &regex::Regex,
    md_parent: &Path,
    by_name: &HashMap<String, PathBuf>,
) -> String {
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for cap in re.captures_iter(text) {
        let Some(whole) = cap.get(0) else {
            continue;
        };
        let Some(pfx) = cap.get(1) else {
            continue;
        };
        let Some(src_m) = cap.get(2) else {
            continue;
        };
        let Some(sfx) = cap.get(3) else {
            continue;
        };
        let src_raw = src_m.as_str().trim();
        if src_raw.starts_with("http://") || src_raw.starts_with("https://") {
            continue;
        }
        let Some(fname) = basename_from_src(src_raw) else {
            continue;
        };
        let Some(abs_img) = by_name.get(&fname) else {
            continue;
        };
        let Some(rel) = relative_path_str(md_parent, abs_img) else {
            continue;
        };
        let rep = format!("{}{}{}", pfx.as_str(), rel, sfx.as_str());
        replacements.push((whole.start(), whole.end(), rep));
    }
    let mut out = text.to_string();
    for (start, end, rep) in replacements.into_iter().rev() {
        out.replace_range(start..end, &rep);
    }
    out
}

pub async fn fix_markdown_paths(
    markdown_files: Vec<String>,
    images_dir: String,
) -> Result<(), String> {
    let images_dir_path = Path::new(&images_dir);

    if !images_dir_path.exists() {
        return Ok(());
    }

    let mut by_name: HashMap<String, PathBuf> = HashMap::new();
    for entry in fs::read_dir(images_dir_path).map_err(|e| format!("读取图片目录失败: {}", e))?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let p = entry.path();
        if p.is_file() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                by_name.insert(name.to_string(), p);
            }
        }
    }

    let md_img = regex::Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)")
        .map_err(|e| format!("Markdown 图片正则无效: {}", e))?;
    // Rust regex 不支持反引用 \2，拆成引号两种写法
    let html_img_dq = regex::Regex::new(r#"(?i)(<img\s[^>]*\bsrc\s*=\s*")([^"]+)("[^>]*>)"#)
        .map_err(|e| format!("HTML img 正则无效: {}", e))?;
    let html_img_sq = regex::Regex::new(r#"(?i)(<img\s[^>]*\bsrc\s*=\s*')([^']+)('[^>]*>)"#)
        .map_err(|e| format!("HTML img 正则无效: {}", e))?;

    for md_path in markdown_files {
        let path = Path::new(&md_path);
        if !path.exists() {
            continue;
        }

        let md_parent = path
            .parent()
            .ok_or_else(|| format!("无法取得父目录: {}", md_path))?
            .to_path_buf();

        let content =
            fs::read_to_string(path).map_err(|e| format!("读取 Markdown 文件失败: {}", e))?;

        let mut new_content = content.clone();

        for cap in md_img.captures_iter(&content) {
            let alt_text = &cap[1];
            let src_raw = cap[2].trim();
            if src_raw.starts_with("http://") || src_raw.starts_with("https://") {
                continue;
            }
            let Some(fname) = basename_from_src(src_raw) else {
                continue;
            };
            let Some(abs_img) = by_name.get(&fname) else {
                continue;
            };
            let Some(rel) = relative_path_str(&md_parent, abs_img) else {
                continue;
            };
            let old_full = format!("![{}]({})", alt_text, cap[2].trim());
            let new_full = format!("![{}]({})", alt_text, rel);
            new_content = new_content.replace(&old_full, &new_full);
        }

        new_content = patch_html_img_tags(&new_content, &html_img_dq, &md_parent, &by_name);
        new_content = patch_html_img_tags(&new_content, &html_img_sq, &md_parent, &by_name);

        if new_content != content {
            fs::write(&path, &new_content).map_err(|e| format!("写入 Markdown 文件失败: {}", e))?;
        }
    }

    Ok(())
}

fn strip_html_img_tags(text: &str, re: &regex::Regex) -> String {
    re.replace_all(text, "").to_string()
}

pub async fn strip_markdown_images(markdown_files: Vec<String>) -> Result<(), String> {
    let md_img = regex::Regex::new(r"!\[[^\]]*\]\([^)]+\)")
        .map_err(|e| format!("Markdown 图片正则无效: {}", e))?;
    let html_img = regex::Regex::new(r#"(?is)<img\b[^>]*>"#)
        .map_err(|e| format!("HTML img 正则无效: {}", e))?;

    for md_path in markdown_files {
        let path = Path::new(&md_path);
        if !path.exists() {
            continue;
        }
        let content =
            fs::read_to_string(path).map_err(|e| format!("读取 Markdown 文件失败: {}", e))?;
        let mut new_content = md_img.replace_all(&content, "").to_string();
        new_content = strip_html_img_tags(&new_content, &html_img);
        if new_content != content {
            fs::write(path, new_content).map_err(|e| format!("写入 Markdown 文件失败: {}", e))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn strip_markdown_images_removes_markdown_and_html_images() {
        let dir =
            std::env::temp_dir().join(format!("mineru-cli-strip-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.md");
        fs::write(
            &file,
            "before\n![alt](images/a.png)\n<img src=\"b.png\" />\nafter\n",
        )
        .unwrap();

        strip_markdown_images(vec![file.to_string_lossy().to_string()])
            .await
            .unwrap();

        let out = fs::read_to_string(&file).unwrap();
        assert!(!out.contains("![alt]"));
        assert!(!out.contains("<img"));
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        let _ = fs::remove_dir_all(dir);
    }
}
