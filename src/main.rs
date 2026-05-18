mod file_ops;
mod mineru_api;
mod output;
mod pdf_splitter;
mod pipeline;
mod settings;
mod usage_stats;

use clap::{Args, Parser, Subcommand};
use pipeline::{collect_and_validate, run_convert, ConvertOptions};
use settings::{load_settings, save_settings, ApiProfile};

#[derive(Parser)]
#[command(name = "mineru-cli")]
#[command(version)]
#[command(about = "MinerU 文档批量转换 CLI 工具")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Convert(ConvertArgs),
    Validate(ValidateArgs),
    Split(SplitArgs),
    Usage(UsageArgs),
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
}

#[derive(Args)]
struct ConvertArgs {
    #[arg(required = true)]
    inputs: Vec<String>,
    #[arg(short, long)]
    output: Option<String>,
    #[arg(long)]
    use_source_dir: bool,
    #[arg(long)]
    token: Option<String>,
    #[arg(long)]
    api: Option<String>,
    #[arg(long, value_delimiter = ',')]
    types: Vec<String>,
    #[arg(long)]
    no_recursive: bool,
    #[arg(long)]
    pool_size: Option<u32>,
    #[arg(long)]
    split_pages: Option<u32>,
    #[arg(long)]
    delete_split_pdfs: bool,
    #[arg(long)]
    delete_originals: bool,
    #[arg(long)]
    balance_apis: bool,
    #[arg(long)]
    ocr: bool,
    #[arg(long)]
    no_ocr: bool,
    #[arg(long)]
    output_assets: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Args)]
struct ValidateArgs {
    #[arg(required = true)]
    inputs: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    types: Vec<String>,
    #[arg(long)]
    no_recursive: bool,
    #[arg(long)]
    split_pages: Option<u32>,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct SplitArgs {
    pdf: String,
    #[arg(short, long)]
    output: String,
    #[arg(long, default_value_t = 100)]
    pages: u32,
}

#[derive(Args)]
struct UsageArgs {
    #[arg(long)]
    token: Option<String>,
    #[arg(long)]
    api: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum ConfigCommands {
    List,
    AddApi {
        id: String,
        name: String,
        token: String,
        #[arg(long)]
        expires_at: Option<String>,
    },
    UpdateApi {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        expires_at: Option<String>,
    },
    RemoveApi {
        id: String,
    },
    SetActiveApi {
        id: String,
    },
    SetOutput {
        dir: String,
    },
    Set {
        key: String,
        value: String,
    },
    Path,
}

fn init_logging(verbose: bool) {
    let level = if verbose { "info" } else { "warn" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level))
        .format_timestamp_secs()
        .init();
}

fn exit_code_from_summary(summary: &pipeline::ConvertSummary) -> i32 {
    if summary.valid_files == 0 {
        4
    } else if summary.tasks_failed > 0 {
        1
    } else {
        0
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let verbose = matches!(&cli.command, Commands::Convert(args) if args.verbose);
    init_logging(verbose);

    let code = match run(cli).await {
        Ok(code) => code,
        Err((code, message)) => {
            eprintln!("{}", message);
            code
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<i32, (i32, String)> {
    match cli.command {
        Commands::Convert(args) => {
            let settings = load_settings().map_err(|e| (3, e))?;
            let options = ConvertOptions {
                inputs: args.inputs,
                output_dir: args.output.or(settings.output_dir.clone()),
                use_source_dir: args.use_source_dir || settings.use_source_dir_as_output,
                token: args.token,
                api_id: args.api,
                types: args.types,
                recursive: !args.no_recursive,
                pool_size: args.pool_size.unwrap_or(settings.api_request_pool_size),
                split_pages: args.split_pages.unwrap_or(settings.pdf_split_pages),
                delete_split_pdfs: args.delete_split_pdfs || settings.delete_split_pdfs_after_done,
                delete_originals: args.delete_originals
                    || settings.delete_original_files_after_done,
                balance_apis: args.balance_apis || settings.balance_load_across_apis,
                dry_run: args.dry_run,
                json: args.json,
                is_ocr: if args.no_ocr {
                    false
                } else {
                    args.ocr || settings.is_ocr
                },
                output_assets: args.output_assets,
            };
            let summary = run_convert(&settings, options).await.map_err(|e| (5, e))?;
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({ "event": "summary", "data": summary })
                );
            } else {
                println!(
                    "完成：有效文件 {}，任务 {}，成功 {}，失败 {}",
                    summary.valid_files,
                    summary.tasks_total,
                    summary.tasks_done,
                    summary.tasks_failed
                );
            }
            Ok(exit_code_from_summary(&summary))
        }
        Commands::Validate(args) => {
            let settings = load_settings().map_err(|e| (3, e))?;
            let options = ConvertOptions {
                inputs: args.inputs,
                output_dir: None,
                use_source_dir: false,
                token: None,
                api_id: None,
                types: args.types,
                recursive: !args.no_recursive,
                pool_size: settings.api_request_pool_size,
                split_pages: args.split_pages.unwrap_or(settings.pdf_split_pages),
                delete_split_pdfs: false,
                delete_originals: false,
                balance_apis: false,
                dry_run: true,
                json: args.json,
                is_ocr: settings.is_ocr,
                output_assets: false,
            };
            let result = collect_and_validate(&options).await.map_err(|e| (4, e))?;
            if args.json {
                println!("{}", serde_json::to_string(&result).unwrap());
            } else {
                println!(
                    "有效 {}，需拆分 {}，无效 {}",
                    result.valid_files.len(),
                    result.files_needing_split.len(),
                    result.invalid_files.len()
                );
                for invalid in result.invalid_files {
                    println!("无效: {}: {}", invalid.name, invalid.reason);
                }
            }
            Ok(0)
        }
        Commands::Split(args) => {
            let result = pdf_splitter::split_pdf(args.pdf, args.output, Some(args.pages))
                .await
                .map_err(|e| (1, e))?;
            println!("拆分完成：{} 个分片", result.chunks.len());
            Ok(0)
        }
        Commands::Usage(args) => {
            let settings = load_settings().map_err(|e| (3, e))?;
            let token = if let Some(t) = args.token {
                t
            } else if let Some(id) = args.api {
                settings
                    .apis
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.token.clone())
                    .ok_or_else(|| (3, format!("未找到 API 配置: {}", id)))?
            } else {
                settings::active_profile(&settings)
                    .map(|p| p.token)
                    .ok_or_else(|| (3, "未配置 API Token".to_string()))?
            };
            let stats = usage_stats::get_usage_stats(token)
                .await
                .map_err(|e| (3, e))?;
            if args.json {
                println!("{}", serde_json::to_string(&stats).unwrap());
            } else {
                println!(
                    "{} 今日用量：{} 页，{} 个任务，Token {}",
                    stats.date, stats.total_pages, stats.total_tasks, stats.token_fingerprint
                );
            }
            Ok(0)
        }
        Commands::Config { command } => run_config(command),
    }
}

fn run_config(command: ConfigCommands) -> Result<i32, (i32, String)> {
    let mut settings = load_settings().map_err(|e| (3, e))?;
    match command {
        ConfigCommands::List => {
            println!("配置文件: {}", settings::config_path().display());
            println!(
                "输出目录: {}",
                settings
                    .output_dir
                    .clone()
                    .unwrap_or_else(|| "-".to_string())
            );
            println!("使用源目录输出: {}", settings.use_source_dir_as_output);
            println!("拆分页数: {}", settings.pdf_split_pages);
            println!("请求池大小: {}", settings.api_request_pool_size);
            println!("多 API 均衡: {}", settings.balance_load_across_apis);
            println!("OCR: {}", settings.is_ocr);
            println!("API:");
            for p in &settings.apis {
                let active = settings.active_api_id.as_deref() == Some(p.id.as_str());
                let expired = settings::is_api_expired(p);
                println!(
                    "  {}{} {} token:{} expires_at:{}{}",
                    if active { "*" } else { "-" },
                    p.id,
                    p.name,
                    if p.token.trim().is_empty() {
                        "empty"
                    } else {
                        "set"
                    },
                    p.expires_at.as_deref().unwrap_or("-"),
                    if expired { " expired" } else { "" }
                );
            }
        }
        ConfigCommands::AddApi {
            id,
            name,
            token,
            expires_at,
        } => {
            validate_expires_at(expires_at.as_deref())?;
            if let Some(existing) = settings.apis.iter_mut().find(|p| p.id == id) {
                existing.name = name;
                existing.token = token;
                existing.expires_at = expires_at;
            } else {
                settings.apis.push(ApiProfile {
                    id: id.clone(),
                    name,
                    token,
                    expires_at,
                });
            }
            if settings.active_api_id.is_none() {
                settings.active_api_id = Some(id);
            }
            save_settings(&settings).map_err(|e| (3, e))?;
        }
        ConfigCommands::UpdateApi {
            id,
            name,
            token,
            expires_at,
        } => {
            validate_expires_at(expires_at.as_deref())?;
            let profile = settings
                .apis
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| (3, format!("未找到 API 配置: {}", id)))?;
            if let Some(name) = name {
                profile.name = name;
            }
            if let Some(token) = token {
                profile.token = token;
            }
            if expires_at.is_some() {
                profile.expires_at = expires_at;
            }
            save_settings(&settings).map_err(|e| (3, e))?;
        }
        ConfigCommands::RemoveApi { id } => {
            settings.apis.retain(|p| p.id != id);
            if settings.active_api_id.as_deref() == Some(id.as_str()) {
                settings.active_api_id = settings.apis.first().map(|p| p.id.clone());
            }
            save_settings(&settings).map_err(|e| (3, e))?;
        }
        ConfigCommands::SetActiveApi { id } => {
            if !settings.apis.iter().any(|p| p.id == id) {
                return Err((3, format!("未找到 API 配置: {}", id)));
            }
            settings.active_api_id = Some(id);
            save_settings(&settings).map_err(|e| (3, e))?;
        }
        ConfigCommands::SetOutput { dir } => {
            settings.output_dir = Some(dir);
            save_settings(&settings).map_err(|e| (3, e))?;
        }
        ConfigCommands::Set { key, value } => {
            match key.as_str() {
                "split-pages" | "pdf_split_pages" => {
                    settings.pdf_split_pages = value
                        .parse::<u32>()
                        .map_err(|_| (2, "split-pages 必须是数字".to_string()))?
                        .clamp(1, 1000);
                }
                "pool-size" | "api_request_pool_size" => {
                    settings.api_request_pool_size = value
                        .parse::<u32>()
                        .map_err(|_| (2, "pool-size 必须是数字".to_string()))?
                        .clamp(1, 100);
                }
                "use-source-dir" | "use_source_dir_as_output" => {
                    settings.use_source_dir_as_output = parse_bool(&value)?;
                }
                "delete-split-pdfs" | "delete_split_pdfs_after_done" => {
                    settings.delete_split_pdfs_after_done = parse_bool(&value)?;
                }
                "delete-originals" | "delete_original_files_after_done" => {
                    settings.delete_original_files_after_done = parse_bool(&value)?;
                }
                "balance-apis" | "balance_load_across_apis" => {
                    settings.balance_load_across_apis = parse_bool(&value)?;
                }
                "ocr" | "is_ocr" => {
                    settings.is_ocr = parse_bool(&value)?;
                }
                _ => return Err((2, format!("未知配置项: {}", key))),
            }
            save_settings(&settings).map_err(|e| (3, e))?;
        }
        ConfigCommands::Path => {
            println!("{}", settings::config_path().display());
        }
    }
    Ok(0)
}

fn parse_bool(value: &str) -> Result<bool, (i32, String)> {
    match value {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err((2, format!("不是布尔值: {}", value))),
    }
}

fn validate_expires_at(value: Option<&str>) -> Result<(), (i32, String)> {
    let Some(value) = value else {
        return Ok(());
    };
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map(|_| ())
        .map_err(|_| (2, "expires-at 必须使用 YYYY-MM-DD 格式".to_string()))
}
