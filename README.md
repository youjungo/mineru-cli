# MinerU Converter

基于 MinerU 精准解析 API 的跨平台文档批量转换 CLI 工具。支持把 PDF、图片、Word、PPT、HTML 批量转换为 Markdown，并自动拆分大 PDF、下载结果、整理 Markdown 与图片目录。

## 功能

- 支持 PDF、图片、Word、PPT、HTML
- PDF 超过拆分页数或 200MB 时自动拆分
- 支持多 API Token 配置与按任务均衡分配
- 自动下载 MinerU 结果 Zip 并整理输出目录
- 自动修正 Markdown 中的图片相对路径
- 本地记录每日用量统计
- 支持人类可读输出和 `--json` JSON Lines 输出

## 安装与构建

开发环境需要 Rust stable。

```bash
cargo build --release
./target/release/mineru-converter --version
```

正式发布通过 GitHub Releases 提供 Windows、Linux、macOS 二进制压缩包。

## 快速使用

临时传入 Token 转换：

```bash
mineru-converter convert ./docs -o ./out --token <MINERU_TOKEN>
```

保存 API 配置后转换：

```bash
mineru-converter config add-api main "Main API" <MINERU_TOKEN>
mineru-converter config set-active-api main
mineru-converter config set-output ./out
mineru-converter convert ./docs
```

只校验输入，不调用 API：

```bash
mineru-converter validate ./docs
mineru-converter convert ./docs -o ./out --dry-run
```

查看用量：

```bash
mineru-converter usage
```

## 常用命令

```bash
mineru-converter convert <inputs...>
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
  --dry-run
  --json
  --verbose
```

配置命令：

```bash
mineru-converter config list
mineru-converter config add-api <id> <name> <token>
mineru-converter config remove-api <id>
mineru-converter config set-active-api <id>
mineru-converter config set-output <dir>
mineru-converter config set split-pages 100
mineru-converter config set pool-size 10
mineru-converter config path
```

## 配置文件

配置文件位于系统配置目录：

- Windows: `%APPDATA%\mineru-converter\config.toml`
- macOS: `~/Library/Application Support/mineru-converter/config.toml`
- Linux: `~/.config/mineru-converter/config.toml`

Token 第一版以明文 TOML 保存。不要把配置文件提交到代码仓库或共享目录。

## 输出结构

转换完成后，每个原始文件会生成一个独立文件夹：

```text
输出目录/
├── 0_文档A/
│   ├── 文档A_1-100页.md
│   ├── 文档A_101-200页.md
│   └── images/
│       ├── image1.png
│       └── image2.jpg
└── 1_文档B/
    ├── 文档B.md
    └── images/
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
