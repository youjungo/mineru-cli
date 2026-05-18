# MinerU CLI CLI - 产品规格说明

## 1. 产品概述

MinerU CLI 是基于 MinerU 精准解析 API 的跨平台 CLI 工具。它面向脚本、批处理和本地自动化场景，将 PDF、图片、Word、PPT、HTML 批量转换为纯 Markdown。

## 2. 核心功能

- 文件输入：支持多个文件和目录输入，目录可递归扫描。
- 类型过滤：支持 `pdf`、`image`、`word`、`ppt`、`html`。
- 预检：检查文件类型、大小、PDF 页数。
- PDF 拆分：PDF 超过配置页数或 200MB 时按页拆分。
- MinerU API：获取上传 URL、PUT 上传、轮询解析结果、下载 Zip。
- 输出整理：每个源文件一个 bundle 目录，默认只保留 Markdown。
- Markdown 修正：默认删除 Markdown 和 HTML 图片引用；`--output-assets` 时保留图片并修正路径。
- 多 API：支持多个 Token、有效期、随机密钥池、超额重试。
- 用量统计：按 Token hash 记录每日任务数和页数，每个 API 默认每日上限 8000 页。
- CLI 输出：支持普通文本和 `--json` JSON Lines。

## 3. 技术架构

- 语言：Rust 2021
- CLI 参数：clap
- 异步运行：tokio
- HTTP：reqwest + rustls
- PDF：lopdf
- Zip：zip
- 配置：TOML，存放于系统配置目录 `mineru-cli/config.toml`

主要模块：

- `file_ops`：文件扫描、类型识别、校验、删除。
- `pdf_splitter`：PDF 分片。
- `mineru_api`：MinerU API 上传与轮询。
- `output`：Zip 下载解压、输出整理、Markdown 图片路径修正。
- `settings`：TOML 配置读写。
- `usage_stats`：本地用量统计。
- `pipeline`：完整转换流程编排。

## 4. 主要命令

```bash
mineru-cli convert <inputs...>
mineru-cli validate <inputs...>
mineru-cli split <pdf> --output <dir>
mineru-cli usage
mineru-cli config <subcommand>
```

`convert` 支持：

```text
--output <dir>
--use-source-dir
--token <token>
--api <id>
--types pdf,image,word,ppt,html
--no-recursive
--pool-size <n>
--split-pages <n>
--delete-split-pdfs
--delete-originals
--balance-apis
--ocr
--no-ocr
--output-assets
--dry-run
--json
--verbose
```

## 5. 数据流

```text
输入文件/目录
  -> 扫描和校验
  -> PDF 按需拆分
  -> 构建转换任务
  -> 随机选择可用 API Token
  -> 上传并轮询 MinerU
  -> API 超额时自动换可用 Token 重试
  -> 下载 Zip
  -> 解压和整理输出
  -> 默认删除 Markdown 图片引用
  -> 记录用量
  -> 可选删除临时分片/源文件
```

## 6. 输出结构

```text
输出目录/
├── 0_文档A/
│   ├── 文档A_1-100页.md
│   └── 文档A_101-200页.md
└── 1_文档B/
    └── 文档B.md
```

## 7. 配置

配置文件为明文 TOML：

```toml
active_api_id = "main"
output_dir = "./out"
delete_split_pdfs_after_done = true
use_source_dir_as_output = false
delete_original_files_after_done = false
api_request_pool_size = 10
pdf_split_pages = 100
balance_load_across_apis = false
is_ocr = true

[[apis]]
id = "main"
name = "Main API"
token = "..."
expires_at = "2026-12-31"
```
