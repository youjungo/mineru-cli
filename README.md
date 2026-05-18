# MinerU CLI

基于 MinerU 精准解析 API 的跨平台文档批量转换 CLI 工具。支持把 PDF、图片、Word、PPT、HTML 批量转换为纯 Markdown，并自动拆分大 PDF、下载结果、移除图片引用。

## 功能

- 支持 PDF、图片、Word、PPT、HTML
- PDF 超过拆分页数或 200MB 时自动拆分
- 支持多 API Token 配置、有效期、随机密钥池与超额重试
- 每个 API 每日默认按 8000 页上限自动调度；指定 `--api` 时允许强制使用
- 自动下载 MinerU 结果 Zip 并整理纯 Markdown 输出
- 默认启用 OCR，并删除 Markdown 中的图片引用
- 本地按密钥记录每日用量统计
- 支持人类可读输出和 `--json` JSON Lines 输出

## 安装与构建

开发环境需要 Rust stable。

```bash
cargo build --release
./target/release/mineru-cli --version
```

正式发布通过 GitHub Releases 提供 Windows、Linux、macOS 二进制压缩包。

## 快速使用

临时传入 Token 转换：

```bash
mineru-cli convert ./docs -o ./out --token <MINERU_TOKEN>
```

保存 API 配置后转换：

```bash
mineru-cli config add-api main "Main API" <MINERU_TOKEN> --expires-at 2026-12-31
mineru-cli config set-active-api main
mineru-cli config set-output ./out
mineru-cli convert ./docs
```

只校验输入，不调用 API：

```bash
mineru-cli validate ./docs
mineru-cli convert ./docs -o ./out --dry-run
```

查看用量：

```bash
mineru-cli usage
```

## 常用命令

```bash
mineru-cli convert <inputs...>
  -o, --output <dir>
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

配置命令：

```bash
mineru-cli config list
mineru-cli config add-api <id> <name> <token> --expires-at 2026-12-31
mineru-cli config update-api <id> --token <token> --expires-at 2027-12-31
mineru-cli config remove-api <id>
mineru-cli config set-active-api <id>
mineru-cli config set-output <dir>
mineru-cli config set split-pages 100
mineru-cli config set pool-size 10
mineru-cli config path
```

## 配置文件

配置文件位于系统配置目录：

- Windows: `%APPDATA%\mineru-cli\config.toml`
- macOS: `~/Library/Application Support/mineru-cli/config.toml`
- Linux: `~/.config/mineru-cli/config.toml`

Token 第一版以明文 TOML 保存。不要把配置文件提交到代码仓库或共享目录。

## 输出结构

默认只输出 Markdown，不保存图片，并删除 Markdown 中的图片引用。加 `--output-assets` 后保留图片资源：

```text
输出目录/
├── 0_文档A/
│   ├── 文档A_1-100页.md
│   └── 文档A_101-200页.md
└── 1_文档B/
    └── 文档B.md
```

## 退出码

- `0`：全部成功或 dry-run 成功
- `1`：部分或全部任务失败
- `2`：参数错误
- `3`：配置或 Token 错误
- `4`：输入全部无效
- `5`：网络/API 流程级错误

## License

MIT
