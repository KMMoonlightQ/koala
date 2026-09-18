# Memory extension

独立的文件式知识库，使用 extension API v1。Agent 不链接本包；本包不依赖 Agent、TUI、MCP 或主程序配置。构建时仅依赖通用协议、LLM 和 Markdown 基础包。发布后的安装目录仅需可执行文件和 manifest。

## 安装

```sh
cargo install --path extensions/memory
koala-memory package /tmp/memory-package --memory-workspace "$PWD/.koala"
koala extension-install /tmp/memory-package
```

在 Agent 配置的 `[extensions].manifests` 中加入安装后的 `extension.toml` 路径，重启生效。删除此路径即停用；文件不会删除。已有知识库可继续使用，无需修改格式。

`package` 从实际工具定义生成 manifest，声明 `turn_start` 和三个工具：

- `memory_search`：只读 BM25 检索。
- `memory_read`：只读文件范围读取，拒绝目录逃逸及符号链接。
- `memory_write`：写入 daily/digest，需 Agent 按权限模式审批，plan mode 不允许。

每轮开始自动检索 5 条片段，最多注入 8,000 字符。工具调用重新读取文件索引，可看到外部修改。不会自动蒸馏或整合。

## 独立 CLI

```sh
koala-memory --workspace .koala search "所有权" -k 5
koala-memory --workspace .koala read digest/wiki/rust.md --start 1 --end 20
koala-memory --config memory.toml distill /absolute/path/session.jsonl
koala-memory --config memory.toml dream
```

`memory.toml` 示例：

```toml
[memory]
workspace = ".koala"
[llm]
base_url = "https://api.openai.com/v1"
model = "your-model"
# 推荐通过 KOALA_API_KEY 环境变量提供密钥
```

兼容 `KOALA_WORKSPACE`、`KOALA_BASE_URL`、`KOALA_API_KEY`、`KOALA_MODEL`。
显式 `--workspace` 优先。配置文件必须通过 `--config` 指定；不自动发现主程序配置。
相对 workspace 相对于调用目录（进程协议模式下为宿主提供的 `KOALA_EXTENSION_CWD`）。
打包会将 workspace 固化为绝对路径，安装到不同目录不会改变数据位置。

转录是通用 JSONL，每行读取 `role`、`content` 字段。空内容及损坏行跳过，UTF-8 截断保持字符完整；不依赖 Agent 的 Rust 类型。

## 协议与测试

`koala-memory serve` 接收一个 API v1 JSON 请求，stdout 返回一个 JSON 响应，错误写 stderr 并非零退出。工具业务错误通过 `is_error` 返回。未知 API 版本拒绝执行。

```sh
cargo test --manifest-path extensions/memory/Cargo.toml
```

进程集成测试会真实打包、安装、删除原包、加载 manifest，验证读写、自动检索、外部修改、停用及版本拒绝。LLM 维护测试使用本机 mock 服务，不访问真实模型。

### 记忆文件与增量整合

`distill` 保留已有卡片：同一天出现同名主题时，新卡片自动使用 `-2`、`-3` 等后缀，
不会覆盖其他会话或之前的蒸馏结果。重复蒸馏同一个会话也会生成新卡片。

`dream` 使用 daily 文件内容的 SHA-256 指纹检测变化；仅修改文件时间不会重复整合，
内容变化即使修改时间未变也会处理。旧版秒级时间戳检查点仍可读取，对应文件会在升级后
重新处理一次，成功整合后保存内容指纹；失败的文件保持待处理状态。
