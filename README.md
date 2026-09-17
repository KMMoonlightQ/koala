# kb-agent

带文件式知识库的终端 Agent。标准 Agent 架构：ReAct 循环 + 工具注册表 + hooks +
权限控制 + 动态 system prompt + plan mode(todo) + skills + 上下文压缩 + 子 Agent +
Agent 私有记忆 + 重试容错 + 后台任务。

**知识库与 Agent 解耦**：Agent 不知道知识库的存在——它只把会话转录（jsonl）追加到
`session_dir`，这是两侧唯一的接口。知识库的检索、蒸馏、整合全部走 CLI 子命令，
由用户显式触发。

## 配置与启动

内置 OpenAI 兼容客户端（`src/llm/mod.rs`：流式 SSE + tool calls），直连任何
OpenAI 兼容端点（OpenAI 官方、DashScope compatible-mode、DeepSeek、本地
vLLM/Ollama 等），Agent 对话和知识库子命令（distill/dream）共用同一份配置。
只有一个配置文件、一条启动命令：

```bash
cp config.example.toml config.toml   # 或 ~/.config/kb-agent/config.toml
# 编辑 config.toml：[llm] 段填 base_url / api_key / model

cargo build --release
./target/release/kb-agent            # 进入对话
```

环境变量 `KBA_BASE_URL` / `KBA_API_KEY` / `KBA_MODEL` / `KBA_WORKSPACE` 可覆盖
文件配置。端点若要求额外的路由/鉴权头（如 opencode zen 的 `x-opencode-session`），
在 `[llm.headers]` 表里声明即可。权限规则、hooks 见 `config.example.toml` 注释。

## Agent 架构

| 组件 | 实现 |
|---|---|
| ReAct 循环 | `agent/react.rs`：流式思考 → 工具调用 → 观察回填，直到模型不再调工具 |
| Tools | `agent/tools/`：trait + 注册表。内置 remember / todo_write / skill / task / bash |
| Hooks | `agent/hooks.rs`：PreToolUse（exit 2 阻断）/ PostToolUse / TurnStart / TurnEnd，stdin 收 JSON |
| 权限控制 | `agent/permissions.rs`：allow/ask/deny 规则；ask 在独立确认区显示操作与参数 |
| System prompt | `agent/prompt.rs`：每次调用前按节组装（基础准则 + agent 私有记忆 + skills 清单 + todos + plan mode） |
| Plan mode | `/plan` 切换；只读工具 + todo_write 规划，写类工具被拒绝 |
| Skills | `./skills/*/SKILL.md` 或 `<config dir>/kb-agent/skills/*/SKILL.md`，清单进 prompt，`skill` 工具按需加载全文 |
| Context compact | history 超阈值自动压缩（保留最近 4 条，其余 LLM 摘要），也可 `/compact` 手动 |
| Sub agents | `task` 工具派生子 Agent（独立上下文，不能再派孙 Agent），支持后台运行 |
| Agent 私有记忆 | `agent.memory_file`（默认 `<config dir>/kb-agent/memory.md`），只给 Agent 用，每轮注入 prompt，`remember` 工具追加 |
| 异常与重试 | LLM 建连失败（429/5xx/网络）指数退避重试 3 次；工具错误作为结果回喂不中断循环 |
| 后台任务 | `bash` / `task` 加 `background=true`，`/tasks` 查看状态，完成自动通知 |

## 记忆

- **知识库**（共享、可人工编辑）：`.kb/{daily,digest}`，与 Agent 完全解耦。Agent 会话只把原始转录写进 `agent.session_dir`（默认 `.kb/session`）；知识库侧通过 CLI 子命令拉取这些转录：`kb-agent distill` 把会话蒸馏成 daily 卡片，`kb-agent dream` 把变化的 daily 卡片整合成 digest 长期记忆（CREATE / CORROBORATE / REFINE / CORRECT），`kb-agent search` / `kb-agent read` 供人检索阅读。
- **Agent 私有记忆**：`agent.memory_file`（默认 `<config dir>/kb-agent/memory.md`），Agent 自用的便签，每轮注入 prompt，与知识库互不引用。

所有知识库记忆都是普通 Markdown：frontmatter 存 `name`/`description`，正文用 `[[路径]]` wikilink 互联。文件是唯一事实源，索引每次启动全量重建。

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

## 子命令（知识库维护，不进对话界面）

```bash
kb-agent search "所有权" -k 5
kb-agent read digest/wiki/ownership.md --start 1 --end 20
kb-agent distill                          # 蒸馏 session_dir 里最近的会话
kb-agent distill path/to/session.jsonl    # 蒸馏指定会话
kb-agent dream                            # 整合长期记忆
```

## 开发

```bash
cargo test                          # 单元测试
cargo clippy --all-targets -- -D warnings
```
