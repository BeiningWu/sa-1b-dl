# sa-1b-dl

批量文件下载工具，支持并发下载、断点续传、HTTP 代理。

## 安装

```bash
cargo build --release
```

## 使用方法

### 基本用法

```bash
# 下载所有文件
sa-1b-dl

# 指定链接文件
sa-1b-dl --link-file links.txt

# 指定输出目录
sa-1b-dl --output /path/to/downloads
```

### 下载模式

```bash
# 下载所有文件 (默认)
sa-1b-dl --mode all

# 下载单个文件
sa-1b-dl --mode single --file sa_000000.tar

# 下载范围文件
sa-1b-dl --mode range --start 0 --end 99
```

### 高级选项

```bash
# 设置并发线程数
sa-1b-dl --threads 8

# 启用断点续传 (默认启用)
sa-1b-dl --resume

# 禁用断点续传
sa-1b-dl --no-resume

# 使用 HTTP 代理
sa-1b-dl --proxy http://127.0.0.1:7890

# 设置重试次数
sa-1b-dl --retries 5
```

## 命令行参数

| 参数 | 短参数 | 默认值 | 说明 |
|------|--------|--------|------|
| `--link-file` | `-l` | `sa-1b_link.txt` | 链接文件路径 |
| `--output` | `-o` | `./my_downloads` | 输出目录 |
| `--mode` | `-m` | `all` | 下载模式: all/single/range |
| `--file` | `-f` | - | 单文件模式时指定文件名 |
| `--start` | - | - | 范围下载起始索引 |
| `--end` | - | - | 范围下载结束索引 |
| `--threads` | `-t` | `4` | 并发下载线程数 |
| `--resume` | - | `true` | 启用断点续传 |
| `--no-resume` | - | - | 禁用断点续传 |
| `--proxy` | - | - | HTTP 代理地址 |
| `--retries` | `-r` | `3` | 下载失败时的重试次数 |

## 链接文件格式

链接文件应为 tab 分隔的文本文件:

```
file_name    url
sa_000000.tar    https://example.com/sa_000000.tar
sa_000001.tar    https://example.com/sa_000001.tar
```

## 项目结构

```
src/
├── main.rs        # 程序入口
├── cli.rs         # CLI 参数解析
├── models.rs      # 数据模型
├── downloader.rs  # 下载逻辑
└── state.rs       # 状态管理
```

## License

MIT
