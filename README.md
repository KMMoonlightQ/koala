# kb-agent

带文件式知识库的终端 Agent。标准 Agent 架构：ReAct 循环 + 工具注册表 + hooks +
权限控制 + 动态 system prompt + plan mode(todo) + skills + 上下文压缩 + 子 Agent +
Agent 私有记忆 + 重试容错 + 后台任务。

**知识库以 extension 接入**：Agent 核心只依赖通用扩展协议。默认启用的 memory extension
在每轮开始时检索相关知识，并提供 memory_search / memory_read / memory_write 工具。
关闭扩展后不注入知识库上下文、不注册知识库工具。蒸馏和整合保留 CLI 入口。

## 快速开始

需要支持 Rust 2024 edition 的 Rust 工具链（含 Cargo），以及支持流式响应和工具调用的
OpenAI 兼容 Chat Completions 端点。

在项目根目录执行：

```bash
cp config.example.toml config.toml
# 编辑 config.toml：[llm] 段填 base_url / api_key / model

cargo build --release
./target/release/kb-agent            # 进入对话，等同于 kb-agent chat
```

也可以安装到 Cargo 的可执行文件目录，后续直接使用 `kb-agent`：

```bash
cargo install --path .
kb-agent --help
```

使用此方式需将 Cargo 的可执行文件目录加入 `PATH`。下文示例使用 `kb-agent`；
未安装时可替换为项目内的 `./target/release/kb-agent`。

## 配置

配置按以下顺序读取，找到第一个文件后停止，不合并多个文件：

1. 当前工作目录的 `config.toml`。
2. 系统配置目录下的 `kb-agent/config.toml`。
3. 如果都不存在，使用内置默认值。

系统配置目录通过 `dirs::config_dir()` 获取：Linux 通常为 `~/.config`（可由
`XDG_CONFIG_HOME` 覆盖），macOS 为 `~/Library/Application Support`，Windows 为
Roaming AppData 目录。全局配置、Agent 私有记忆和全局 skills 均使用此目录。

最小配置示例：

```toml
[llm]
base_url = "https://api.openai.com/v1"
api_key = "your-api-key"
model = "your-model-name"
```

`base_url` 填 API 基础路径，客户端会追加 `/chat/completions`。对话、`distill` 和
`dream` 共用 `[llm]` 配置；`search` 和 `read` 在本地运行，不需要调用模型。
端点要求额外请求头时，可在 `[llm.headers]` 中声明。

以下环境变量覆盖文件配置：

| 环境变量 | 对应配置 | 默认值 |
|---|---|---|
| `KBA_BASE_URL` | `llm.base_url` | `https://api.openai.com/v1` |
| `KBA_API_KEY` | `llm.api_key` | 空 |
| `KBA_MODEL` | `llm.model` | 空，调用模型前需配置 |
| `KBA_WORKSPACE` | `memory.workspace` | `.kb` |

`KBA_WORKSPACE` 只改变知识库目录。会话转录目录由 `agent.session_dir` 单独控制，
默认仍为 `.kb/session`；相对路径均相对于启动时的工作目录。

权限按工具名匹配，优先级为 `allow` → `deny` → `default`。默认允许
`todo_write`、`remember` 和 `skill`，其余工具请求确认。
完整的 Agent 参数、权限规则和 hooks 示例见 [config.example.toml](config.example.toml)。

## 记忆

- **知识库 extension**（共享、可人工编辑）：`.kb/{daily,digest}`。每轮自动检索最多 5 条相关片段，注入上下文最多 8,000 字符；Agent 可通过扩展工具检索、阅读和保存知识。Agent 仍把原始转录写进 `agent.session_dir`（默认 `.kb/session`）；`kb-agent distill` 把会话蒸馏成 daily 卡片，`kb-agent dream` 把变化的 daily 卡片整合成 digest 长期记忆（CREATE / CORROBORATE / REFINE / CORRECT），`kb-agent search` / `kb-agent read` 供人检索阅读。
- **Agent 私有记忆**：`agent.memory_file`（默认 `<config dir>/kb-agent/memory.md`），Agent 自用的便签，每轮注入 prompt，与知识库互不引用。

所有知识库记忆都是普通 Markdown：frontmatter 存 `name`/`description`，正文用 `[[路径]]` wikilink 互联。文件是唯一事实源，索引每次启动全量重建。

默认知识库目录结构：

```text
.kb/
├── session/            # 会话转录（默认 agent.session_dir）
├── daily/              # 蒸馏后的日常记忆卡片
├── digest/
│   ├── personal/       # 个人信息与偏好
│   ├── procedure/      # 流程与方法
│   └── wiki/           # 知识条目
└── metadata/           # 知识库维护元数据
```

## 知识库命令

这些子命令直接在终端运行，不进入对话界面。典型流程是：对话产生转录 →
`distill` 生成 daily 卡片 → `dream` 整合 digest → `search` / `read` 检索阅读。

```bash
kb-agent distill                          # 蒸馏 session_dir 中修改时间最新的一个 .jsonl 文件
kb-agent distill path/to/session.jsonl    # 蒸馏指定会话
kb-agent dream                            # 将发生变化的 daily 卡片整合为长期记忆
kb-agent search "所有权" -k 5             # BM25 检索，默认返回最多 5 条
kb-agent read digest/wiki/ownership.md --start 1 --end 20
```

`read` 的路径相对于 `memory.workspace`，需替换为实际存在的文件路径；行号从 1 开始，
包含起止行。不传 `--start` / `--end` 时读取全文。`search` 输出文件路径、行号范围、
相关度分数和匹配内容。

## 对话内命令

| 输入 | 作用 |
|---|---|
| `/new` | 中断当前前台工作并开新会话，同步清空对话视图 |
| `/plan` | 开关 plan mode |
| `/compact` | 手动压缩上下文 |
| `/tasks` | 查看后台任务 |
| `/todos` | 展开或收起当前 Todo |
| `/help` | 打开快捷键与命令帮助 |
| `/skills` | 列出已加载 skills |
| `/quit` 或 `/q` | 退出（可在执行中使用） |

Enter 发送，Shift+Enter、Ctrl+J 或 `\` 后接 Enter 换行。启用终端的 bracketed paste，
整段粘贴只插入草稿，不会自动发送。执行中按 Enter 会保留草稿并显示提示，不会丢弃输入。

- `Esc`：中断当前响应、工具或压缩操作；权限确认区打开时，拒绝本次操作。
- `Ctrl+C`：执行中中断整轮；空闲时清空输入。
- `Ctrl+D`：输入为空时退出；有草稿时交给输入框处理，不直接退出。
- `PageUp` / `PageDown`：翻阅历史。上翻后新内容不会抢走阅读位置，翻到底部后恢复跟随。
- `Ctrl+End`：回到底部并恢复跟随。
- `/`：显示带说明的命令菜单，继续输入可筛选；上下键选择、Tab 补全、Enter 执行、Esc 收起。
  未知命令会显示提示并保留输入。
- `↑` / `↓`：多行输入中先移动光标，到首行或末行后召回历史；向下越过最新历史会恢复原草稿。
- `Ctrl+R`：搜索当前工作目录的输入历史，输入关键词筛选，上下键或 Ctrl+R 选择，
  Enter / Tab 只回填输入框，Esc 取消并保留原草稿。
- `Shift+Tab`：空闲时切换 Normal / Plan，保留草稿；执行中提示先中断，模式不会悄悄改变。
- `Ctrl+T`：展开或收起 Todo 区，优先展示进行中的项目；完整列表可在 Ctrl+O 详细记录中查看。
- 空输入时 `?` 或 `/help`：打开帮助，支持滚动，Esc 返回。
- `Ctrl+O`：切换详细记录，展开工具完整参数与返回内容。详情中用上下键、PageUp / PageDown
  滚动，Home / End 跳转首尾；Esc、q 或 Ctrl+O 返回，保留草稿及简洁视图的阅读位置。
- 权限确认：方向键或 Tab 选择，Enter 确认，默认选中拒绝；也支持 `y` / `n`。
  无关按键不提交决定，参数较长时可用 PageUp / PageDown 查看。

输入区上方显示当前阶段、耗时和操作提示。中断会保留已显示的对话进展，
可继续输入；已完成的操作不会撤销。在 Unix 上，前台 shell 及同组子进程会随中断终止。
显式启动的后台任务继续运行，`/new` 后旧会话的后台通知不会插入新对话。

`/tasks` 在前台执行期间也可打开：上下键选择任务，Enter 查看完整输出，`x` 停止选中任务。
列表显示状态和耗时并自动更新；输出在任务结束后提供。输出页 Esc 返回列表，再按 Esc 返回对话。
停止操作等待任务取消完成再确认，前台工作和其他后台任务不受影响。

已提交的输入按工作目录保存在 `agent.session_dir/.input-history`，可跨次启动召回和搜索，
界面保留最近 1,000 条，连续重复提交去重。新会话不清除输入历史；未发送草稿不会写入历史。

## 对话界面

启动区显示版本、模型和实际工作目录。底部使用简洁的分隔线输入区，提供空输入提示；
状态栏持续显示模型、Normal / Plan 模式与运行中的后台任务数量。`/plan` 的实际切换结果
由后端同步到状态栏；上下文用量尚无准确数据，因此不显示估算百分比。

助手消息支持 Markdown 标题、强调、列表、任务列表、引用、链接、代码块和表格。
代码块支持 Rust、Python、JS/TS、JSON、Shell 的基础词法着色，其他语言保留原文。
表格在窄窗口中改为带字段名的逐项展示；中文和 emoji 按显示宽度换行。

工具调用使用独立标记区分进行中、成功、失败与中断，并显示耗时和参数摘要。
默认显示最多三行输出，`Ctrl+O` 可查看完整工具返回和参数。前台 shell 输出会完整传给
界面，模型上下文仍接收最多约 8,000 字节的摘录；后台完成通知使用摘要，完整结果可在 `/tasks` 中查看。

## Agent 架构

| 组件 | 实现 |
|---|---|
| ReAct 循环 | `agent/react.rs`：流式思考 → 工具调用 → 观察回填，直到模型不再调工具或达到轮数上限（默认 8） |
| Tools | `agent/tools/`：trait + 注册表。内置 remember / todo_write / skill / task / bash |
| Extensions | `extensions/`：可安装的进程扩展 + 原生 trait；上下文注入、工具注册、参数/结果处理；知识库实现位于 `extensions/memory/` |
| Hooks | `agent/hooks.rs`：PreToolUse（exit 2 阻断）/ PostToolUse / TurnStart / TurnEnd，stdin 收 JSON |
| 权限控制 | `agent/permissions.rs`：allow/ask/deny 规则；ask 在独立确认区显示操作与参数 |
| System prompt | `agent/prompt.rs`：每次调用前按节组装（基础准则 + agent 私有记忆 + skills 清单 + todos + plan mode） |
| Plan mode | `/plan` 切换；允许 todo_write / skill / task 及扩展声明的只读工具；写工具被拒绝，子 Agent 继承此限制 |
| Skills | `./skills/*/SKILL.md` 或 `<config dir>/kb-agent/skills/*/SKILL.md`，清单进 prompt，`skill` 工具按需加载全文 |
| Context compact | history 超阈值自动压缩（保留最近 4 条，其余 LLM 摘要），也可 `/compact` 手动 |
| Sub agents | `task` 工具派生子 Agent（独立上下文，不能再派孙 Agent），支持后台运行 |
| Agent 私有记忆 | `agent.memory_file`（默认 `<config dir>/kb-agent/memory.md`），只给 Agent 用，每轮注入 prompt，`remember` 工具追加 |
| 异常与重试 | LLM 建连失败（429/5xx/网络）指数退避重试 3 次；工具错误作为结果回喂不中断循环 |
| 后台任务 | `bash` / `task` 加 `background=true`，`/tasks` 查看状态，完成自动通知 |

## 开发

```bash
cargo fmt --check                   # 格式检查
cargo test                          # 测试
cargo clippy --all-targets -- -D warnings
```


## Extension 开发与安装

扩展是可信的本地程序，可读写本机文件、访问网络；它与 Agent 具有相同的系统权限。
工具权限控制只约束 Agent 发起的工具调用，不是扩展进程的沙箱。

安装仓库内的示例扩展：

```bash
kb-agent extension-install examples/extensions/context
```

命令复制目录至 `.kb/extensions/<name>`，拒绝覆盖已有安装、符号链接和特殊文件。
随后把命令输出的 manifest 路径加入配置（重启生效）：

```toml
[extensions]
memory = true
manifests = [".kb/extensions/context-example/extension.toml"]
timeout_secs = 30
```

从 `manifests` 删除路径即停用外部扩展；`memory = false` 完全关闭知识库的 Agent 集成，
不会影响已有文件及独立 CLI 命令。Agent 私有 `remember` 便签仍独立存在。

扩展目录必须包含 `extension.toml`，也可不复制目录而直接配置 manifest 路径：

```toml
api_version = 1
name = "my-extension"
command = ["python3", "main.py"]
hooks = ["turn_start", "before_model", "after_model", "pre_tool_use",
         "post_tool_use", "before_compact", "after_compact", "turn_end"]

[[tools]]
name = "lookup_example"
description = "Look up a local record"
read_only = true
parameters = { type = "object", properties = { query = { type = "string" } }, required = ["query"] }
```

每次调用启动独立进程，工作目录为 manifest 所在目录；跨调用状态须自行持久化。
`command` 为 argv，不经过 shell 展开。stdin 接收一个 JSON 对象，stdout 必须返回一个
JSON 对象（无修改时 `{}`）；日志写 stderr。协议版本当前为 1。

- Hook 请求：`{"api_version":1,"kind":"hook","stage":"pre_tool_use","payload":{"tool":"bash","arguments":{"command":"pwd"},"depth":0,"plan_mode":false}}`。
- 工具请求：`{"api_version":1,"kind":"tool","name":"lookup_example","arguments":{"query":"rust"}}`。
- 响应字段：`context` 注入上下文；`block` 阻断；`arguments` 替换工具参数对象；`content` 和 `is_error` 返回或修改工具结果。

| 阶段 | payload 主要字段 | 可用响应 |
|---|---|---|
| turn_start | input、depth、plan_mode；主 Agent 另有 session | context、block |
| before_model | messages、tools、depth、plan_mode | context、block |
| after_model | message、depth、plan_mode | 观察；block 会终止本轮 |
| pre_tool_use | tool、arguments、depth、plan_mode | arguments、block |
| post_tool_use | tool、原始 arguments、content、is_error、depth、plan_mode | content、is_error |
| before_compact | session、messages | block |
| after_compact | session、messages、changed | 观察 |
| turn_end | input、reply 或 error、depth、plan_mode；主 Agent 成功时另有 session、session_path | 观察 |

memory 最先执行，外部扩展按配置顺序执行，后续扩展可看到前序改写的参数和结果。
`before_model` 的上下文只作用于当前请求，不累积到历史。`pre_tool_use` 改写后再运行
原 shell pre hook、plan mode 校验和权限确认，扩展不能通过改参数跳过审批。
扩展工具不可覆盖内置工具，也不可与其他扩展重名。只有明确声明 `read_only = true`
的扩展工具允许在 plan mode 使用；普通权限规则仍然适用。

进程非零退出、非法 JSON 和超时均视为扩展错误；前置阶段失败会停止对应操作，
post_tool_use / turn_end 失败只显示诊断，不抹掉已完成的结果。中断会杀死扩展进程组；
强制中断不会保证调用 turn_end。子 Agent 继承已加载扩展和 plan mode，拥有独立轮次，
不提供主会话转录路径。不要在子 Agent 的 turn_end 中假设 session_path 一定存在。

知识库扩展通过 turn_start 自动检索，通过工具实时打开文件库，因此可看到外部文件修改。
`memory_write` 遵守权限确认与 plan mode；`distill` / `dream` 仍显式运行，不会在每轮
结束时隐式调用模型或整合文件。Rust 扩展也可实现 `extensions::Extension` 并通过
`Extensions::register` 接入同一协议。原 `kb_agent::memory` 模块路径保留兼容导出。
