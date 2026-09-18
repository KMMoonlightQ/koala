# koala

基于 Rust 的终端 AI Agent，支持文件式知识库、历史会话恢复、模型与思考档位切换、
三级工具权限和中英文界面。通过 ReAct 循环调用工具，提供 Plan 模式、Todo、skills、
上下文压缩、子 Agent 和后台任务。

**知识库以 extension 接入**：Agent 核心只依赖通用扩展协议。独立安装的 memory extension
在每轮开始时检索相关知识，并提供 memory_search / memory_read / memory_write 工具。
移除 manifest 配置即关闭集成，不注册知识库工具。知识库由独立的 `koala-memory` 程序提供；
Agent 的依赖图不包含 memory 包，删除 `extensions/memory/` 也能构建 Agent。

## 目录

- [快速开始](#快速开始)
- [配置](#配置)
- [记忆](#记忆)与[知识库命令](#知识库命令)
- [界面语言](#界面语言)与[对话内命令](#对话内命令)
- [会话恢复](#会话恢复)、[工具权限](#工具权限)与[模型和思考档位](#模型和思考档位)
- [快捷键与输入](#快捷键与输入)、[后台任务](#后台任务)与[对话界面](#对话界面)
- [Agent 架构](#agent-架构)
- [文件工具与系统提示词](#文件工具与系统提示词)
- [MCP 服务器](#mcp-服务器)
- [Extension 开发与安装](#extension-开发与安装)
- [开发](#开发)

## 快速开始

需要支持 Rust 2024 edition 的 Rust 工具链（含 Cargo），以及支持流式响应和工具调用的
OpenAI 兼容 Chat Completions 端点。

在项目根目录执行：

```bash
cp config.example.toml config.toml
# 编辑 config.toml：[llm] 段填 base_url / api_key / model

cargo build --release
./target/release/koala            # 进入对话，等同于 koala chat
```

也可以安装到 Cargo 的可执行文件目录，后续直接使用 `koala`：

```bash
cargo install --path .
koala --help
```

使用此方式需将 Cargo 的可执行文件目录加入 `PATH`。下文示例使用 `koala`；
未安装时可替换为项目内的 `./target/release/koala`。

## 配置

配置按以下顺序读取，找到第一个文件后停止，不合并多个文件：

1. 当前工作目录的 `config.toml`。
2. 系统配置目录下的 `koala/config.toml`。
3. 如果都不存在，使用内置默认值。

系统配置目录通过 `dirs::config_dir()` 获取：Linux 通常为 `~/.config`（可由
`XDG_CONFIG_HOME` 覆盖），macOS 为 `~/Library/Application Support`，Windows 为
Roaming AppData 目录。全局配置、Agent 私有记忆和全局 skills 均使用此目录。
统一使用 `koala` 配置子目录、`KOALA_*` 环境变量和 `.koala` 数据目录。
从旧版本升级时，请迁移数据和全局配置目录，并同步更新自定义路径及环境变量名称。

最小配置示例：

```toml
[llm]
base_url = "https://api.openai.com/v1"
api_key = "your-api-key"
model = "your-model-name"
```

`base_url` 填 API 基础路径，客户端会追加 `/chat/completions`。对话使用 Agent 的 `[llm]` 配置。独立 memory 的 `distill` / `dream` 使用其显式指定的配置；
`search` 和 `read` 在本地运行，不需要调用模型。
端点要求额外请求头时，可在 `[llm.headers]` 中声明。

以下环境变量覆盖文件配置：

| 环境变量 | 对应配置 | 默认值 |
|---|---|---|
| `KOALA_BASE_URL` | `llm.base_url` | `https://api.openai.com/v1` |
| `KOALA_API_KEY` | `llm.api_key` | 空 |
| `KOALA_MODEL` | `llm.model` | 空，调用模型前需配置 |
| `KOALA_LANG` | 顶层 `lang`（`en` / `zh`） | `en` |
| `KOALA_THEME` | 顶层 `theme`（`auto` / `light` / `dark`） | `auto` |

`KOALA_WORKSPACE` 仅由独立的 memory 扩展读取，Agent 不读取该变量。会话转录目录由 `agent.session_dir` 单独控制，
默认仍为 `.koala/session`；相对路径均相对于启动时的工作目录。

工具权限默认使用 **Normal**，每次工具调用均需确认；可通过 `/permissions` 切换。
`deny` 在 Normal / Ask When Need 下优先，`allow` 仅在 Ask When Need 下生效；
Never Ask 自动放行全部工具。具体行为见[工具权限](#工具权限)。
完整的 Agent 参数、权限规则和 hooks 示例见 [config.example.toml](config.example.toml)。

## 记忆

- **知识库 extension**（共享、可人工编辑）：`.koala/{daily,digest}`。每轮自动检索最多 5 条相关片段，注入上下文最多 8,000 字符；Agent 可通过扩展工具检索、阅读和保存知识。Agent 仍把原始转录写进 `agent.session_dir`（默认 `.koala/session`）；`koala-memory distill` 把会话蒸馏成 daily 卡片，`koala-memory dream` 把变化的 daily 卡片整合成 digest 长期记忆（CREATE / CORROBORATE / REFINE / CORRECT），`koala-memory search` / `koala-memory read` 供人检索阅读。
- **Agent 私有记忆**：`agent.memory_file`（默认 `<config dir>/koala/memory.md`），Agent 自用的便签，每轮注入 prompt，与知识库互不引用。

所有知识库记忆都是普通 Markdown：frontmatter 存 `name`/`description`，正文用 `[[路径]]` wikilink 互联。文件是唯一事实源，索引每次启动全量重建。

默认知识库目录结构：

```text
.koala/
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
koala-memory distill path/to/session.jsonl    # 蒸馏指定会话
koala-memory dream                            # 将发生变化的 daily 卡片整合为长期记忆
koala-memory search "所有权" -k 5             # BM25 检索，默认返回最多 5 条
koala-memory read digest/wiki/ownership.md --start 1 --end 20
```

`distill` 遇到同一天的同名卡片时自动添加 `-2`、`-3` 等后缀，不覆盖已有记忆。
`dream` 按内容指纹检测变化；旧版时间戳检查点会在下次成功整合后迁移，首次会重新处理对应卡片。

`read` 的路径相对于扩展配置的 `memory.workspace`，需替换为实际存在的文件路径；行号从 1 开始，
包含起止行。不传 `--start` / `--end` 时读取全文。`search` 输出文件路径、行号范围、
相关度分数和匹配内容。

## 界面语言

界面文案、系统提示词和 Agent 侧提示都走同一张映射表（`src/i18n.rs`），
默认英文。三种切换方式：`/lang` 在对话中切换（`/lang en` / `/lang zh` 显式指定），
`config.toml` 顶层（放在 `[llm]` 等表声明之前）写 `lang = "zh"` 设置启动语言，
或用环境变量 `KOALA_LANG` 覆盖。

`/lang` 会立即将选择保存到当前工作目录的 `.koala/language.toml`，在同一目录
退出重开后仍生效。启动语言优先级：`KOALA_LANG` > 已保存的语言 > `config.toml`
> 默认英文。删除 `.koala/language.toml` 可恢复使用配置文件的语言；保存失败会在界面提示。

## 界面主题

默认 `auto`：使用终端自身的默认前景色、背景色和 ANSI 调色板，终端切换配色时自动跟随，
无需重启。`light` 使用固定浅色配色，`dark` 使用固定深色配色。

输入 `/theme auto`、`/theme light` 或 `/theme dark` 立即切换，执行任务期间也可使用。
单独输入 `/theme` 弹出主题选择窗，默认选中当前主题；方向键选择、Enter 确认并保存、Esc 取消。选择保存到当前目录的 `.koala/theme.toml`，重启后保留。
也可在 `config.toml` 顶层设置 `theme = "auto"`，或使用环境变量 `KOALA_THEME`。
优先级：`KOALA_THEME` > 已保存的主题 > `config.toml` > `auto`。

## 对话内命令

| 输入 | 作用 |
|---|---|
| `/sessions` | 展示历史会话，↑↓ 选择、Enter 恢复并继续对话、Esc 取消 |
| `/new` | 中断当前前台工作并开新会话，同步清空对话视图 |
| `/plan` | 开关 plan mode |
| `/compact` | 手动压缩上下文 |
| `/model` | 打开模型列表，↑↓ 选择、Enter 确认、Esc 取消；也支持 `/model 模型名` |
| `/permissions` | 选择四级权限；也支持 `/permissions normal`、`/permissions ask_when_need`、`/permissions auto_edit`、`/permissions never_ask` |
| `/effort` | 打开思考档位列表，↑↓ 选择、Enter 确认、Esc 取消；也支持 `/effort high` |
| `/tasks` | 查看后台任务 |
| `/todos` | 打开完整 Todo 列表 |
| `/help` | 打开快捷键与命令帮助 |
| `/skills` | 列出已加载 skills |
| `/theme` | 弹窗选择主题；`/theme auto`、`/theme light`、`/theme dark` 切换并保存主题 |
| `/lang` | 切换界面语言（中文 / English），也支持 `/lang en`、`/lang zh`；可在执行中使用 |
| `/quit` 或 `/q` | 退出（可在执行中使用） |

## 会话恢复

退出后再次启动，输入 `/sessions` 即可查找并恢复历史会话。列表读取当前配置的
`agent.session_dir`（默认 `.koala/session`，相对于启动工作目录），按最近更新时间排序，
显示首条用户消息、更新时间、会话 ID，并标记当前会话。Enter 恢复历史对话到界面和模型上下文，
后续消息继续追加到原文件。前台执行期间需先中断，再切换会话。

恢复时权限回到 **Normal**、退出 Plan 模式，保留当前模型设置。新会话在原 `.jsonl` 问答转录之外，
使用同名 `.work` 日志保存工具参数、完整显示输出、调用状态与耗时、模型工具消息、Todo 和后台任务状态。
恢复后工具详情和 Todo 回到界面，模型也能接续工具上下文；旧版仅含 `.jsonl` 的会话仍可恢复聊天。
同一进程内切回会话会重新关联仍在运行的后台任务；应用重启后保留已保存的任务结果，原运行中任务显示为已停止，
并注明执行中断、结果未知，不会自动重跑命令。同一进程中，离开会话期间完成的任务通知会在切回时显示一次。首轮尚未完成的会话也可从工作日志恢复。
损坏或无法读取的会话会显示错误，当前对话保持不变。切换会话不会重新执行历史操作。

## 工具权限

权限等级与 Normal / Plan 工作模式独立，底栏始终显示当前权限。默认 **Normal**：

| 等级 | 行为 |
|---|---|
| Normal | 所有工具调用都需审批，包括读取、Todo 和记忆操作 |
| Ask When Need | 自动执行 read、Todo、Agent 私有记忆追加、加载 skill、派生任务及扩展声明的只读工具；edit、write 和危险或无法判定的操作需审批 |
| Auto Edit | 在 Ask When Need 基础上，自动执行当前工作区内的 edit/write；工作区外路径及指向外部的符号链接仍需审批，Shell 和扩展写操作规则不变 |
| Never Ask | 所有工具调用自动执行，不再请求审批 |

Ask When Need 仅自动放行简单的 `pwd`、`ls`、`cat`、`head`、`tail`、`wc` shell 命令。
重定向、管道、命令组合、变量展开、脚本执行、网络请求及其他无法确认安全的命令都会请求审批。
Auto Edit 的工作区为启动目录，路径按真实目标解析（包括符号链接）；支持在工作区内新建子目录和文件。
显式 `allow` 仍可授权整个工具（如 `write`），包括工作区外操作；`deny` 优先。
子 Agent 的后续工具调用仍逐次检查相同权限；没有交互通道时，需要审批的操作会被拒绝。

`/permissions` 在前台空闲时切换，影响主 Agent 和已启动子 Agent 的后续工具调用；
已启动的操作不会因此停止。切换在 `/new` 后保留，重启后使用配置值：

```toml
[permissions]
mode = "normal" # normal / ask_when_need / auto_edit / never_ask
allow = []      # Ask When Need / Auto Edit：明确授权可自动执行的工具
deny = []      # Normal / Ask When Need / Auto Edit：直接禁止的工具，优先于 allow
```

Never Ask 忽略 `allow` / `deny`；Plan 模式限制和 hook 阻断仍然有效。
旧配置的 `permissions.default` 已由 `permissions.mode` 替代，需要删除 `default` 并选择新等级；
旧字段或拼写错误会报配置错误，避免静默改变权限。

## 模型和思考档位

底栏显示权限模式、模型、思考档位和上下文已用百分比，未配置的项目隐藏。例如在已有的
`[llm]` 段中添加（数值和档位需按当前模型及服务商的实际能力填写）：

```toml
reasoning_efforts = ["low", "medium", "high"]
reasoning_effort = "medium"
context_window = 128000
```

`reasoning_effort` 必须在 `reasoning_efforts` 中；省略时使用列表第一项。未配置列表时不发送
`reasoning_effort` 参数。`context_window` 是正整数，单位为 tokens，用于计算最近一次请求的上下文占用率（输入加输出 token 数 / 容量），
不改变压缩阈值。`/effort` 在前台空闲时可用，切换对后续模型请求生效（包括共享客户端的
子任务及压缩请求），不会修改已发出的请求。选择只保留在本次进程中，`/new` 不重置，重启后恢复配置。

使用 `[[llm.models]]` 添加 `/model` 的可选模型，所有模型共用 `[llm]` 的端点、密钥及请求头：

```toml
[[llm.models]]
model = "your-other-model"
reasoning_efforts = ["low", "high"]
reasoning_effort = "high"
context_window = 128000
```

`llm.model` 指定启动模型，自动出现在列表中；如果列表中有同名项，该项的能力配置优先，
否则使用原 `[llm]` 中的能力配置。各模型未配置的能力保持隐藏，不继承其他模型的值。
`/model` 在前台空闲时切换，保留历史和 Todo，更新底栏及 `/effort` 列表，并应用目标模型的
初始思考档位。切换影响共享客户端的后续请求（含后台子任务），不修改已发出的请求。
选择在当前进程内有效，`/new` 保留选择，重启恢复 `llm.model`。

## 快捷键与输入

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
- `Ctrl+T`：打开或关闭完整 Todo 列表，优先展示进行中的项目；↑↓ / PageUp / PageDown 滚动，Esc 返回。`Ctrl+O` 用于详细记录和工具调用展开。
- 空输入时 `?` 或 `/help`：打开帮助，支持滚动，Esc 返回。
- `Ctrl+O`：切换详细记录，展开工具完整参数与返回内容。详情中用上下键、PageUp / PageDown
  滚动，Home / End 跳转首尾；Esc、q 或 Ctrl+O 返回，保留草稿及简洁视图的阅读位置。
- 权限确认：方向键或 Tab 选择，Enter 确认，默认选中拒绝；也支持 `y` / `n`。
  无关按键不提交决定，参数较长时可用 PageUp / PageDown 查看。

已提交的输入按工作目录保存在 `agent.session_dir/.input-history`，可跨次启动召回和搜索，
界面保留最近 1,000 条，连续重复提交去重。新会话不清除输入历史；未发送草稿不会写入历史。

输入区上方显示当前阶段、耗时和操作提示。中断会保留已显示的对话进展，
可继续输入；已完成的操作不会撤销。在 Unix 上，前台 shell 及同组子进程会随中断终止。
显式启动的后台任务继续运行，`/new` 后旧会话的后台通知不会插入新对话。
当前会话已完成、失败或停止的后台任务结果，会在主 Agent 下一次模型请求时自动注入上下文
（每项输出最多约 8,000 字节）；`/new` 后旧任务结果不会注入新会话。

## 后台任务

`/tasks` 在前台执行期间也可打开：上下键选择任务，Enter 查看完整输出，`x` 停止选中任务。
列表显示状态和耗时并自动更新；输出在任务结束后提供。输出页 Esc 返回列表，再按 Esc 返回对话。
停止操作等待任务取消完成再确认，前台工作和其他后台任务不受影响。

## 对话界面

启动区显示像素考拉、koala、版本和工作目录。底部状态栏显示权限等级、模型和已配置的
思考档位及上下文用量，仅在启用 Plan 或存在后台任务时显示对应状态；快捷键说明集中在 `?` 帮助面板。`/plan` 的实际切换结果
由后端同步到状态栏；用量来自流式响应的 `usage`，不会累加历次请求。首次请求前、压缩或恢复会话后、切换模型后，以及接口未返回用量时显示 `--%`；未配置容量时隐藏百分比。

助手消息支持 Markdown 标题、强调、列表、任务列表、引用、链接、代码块和表格。
代码块支持 Rust、Python、JS/TS、JSON、Shell 的基础词法着色，其他语言保留原文。
表格在窄窗口中改为带字段名的逐项展示；中文和 emoji 按显示宽度换行。

工具调用使用独立标记区分进行中、成功、失败与中断，并显示耗时和参数摘要。
默认显示最多三行输出，`Ctrl+O` 可查看完整工具返回和参数。前台 shell 输出会完整传给
界面，模型上下文仍接收最多约 8,000 字节的摘录；后台完成通知使用摘要，完整结果可在 `/tasks` 中查看。

## Agent 架构

| 组件 | 实现 |
|---|---|
| 显示转录 | `src/tui/transcript.rs`：集中处理转录事件、工具状态、待办、滚动、详情模式与渲染缓存；界面通过操作与可见行访问它 |
| 持久化转录 | `src/agent/transcripts.rs`：管理当前转录标识、整轮追加、严格恢复与容错蒸馏；保存失败保留内存回答，避免再次记为中断 |
| ReAct 循环 | `src/agent/react.rs`：流式思考 → 工具调用 → 观察回填，直到模型不再调工具；可配置工具轮数上限，默认不限 |
| 工具目录 | `src/agent/tools/catalog.rs`：统一内置与扩展工具的定义、提示词摘要、使用规则、能力和执行归属；自动许可与 plan mode 可用性分别判断。内置 read / bash / edit / write / remember / todo_write / skill / task |
| Extensions | `crates/extension-api/` 与 `crates/extensions/`：协议及通用运行时；知识库为独立项目 `extensions/memory/` |
| Hooks | `src/agent/hooks.rs`：PreToolUse（exit 2 阻断）/ PostToolUse / TurnStart / TurnEnd，stdin 收 JSON |
| 权限控制 | `src/agent/permissions.rs`：三级可切换审批策略；审批区显示操作与参数 |
| System prompt | `src/agent/prompt.rs`：每次模型请求前组装角色、当前工具及规则、项目指令、skills 目录、实时记忆与 Todo、模式约束和 cwd；支持自定义前缀与追加指令 |
| Plan mode | `/plan` 切换；仅向模型提供 read / todo_write / skill / task 及扩展声明的只读工具；bash、edit、write、remember 的执行仍会被后端拒绝，子 Agent 继承此限制 |
| Skills | `./skills/*/SKILL.md` 或 `<config dir>/koala/skills/*/SKILL.md`，清单进 prompt，`skill` 工具按需加载全文 |
| Context compact | history 超阈值自动压缩（保留最近 4 条，其余 LLM 摘要），也可 `/compact` 手动 |
| Sub agents | `task` 工具派生子 Agent（独立上下文，不能再派孙 Agent），支持后台运行 |
| Agent 私有记忆 | `agent.memory_file`（默认 `<config dir>/koala/memory.md`），只给 Agent 用，每轮注入 prompt，`remember` 工具追加 |
| 异常与重试 | LLM 建连失败（429/5xx/网络）指数退避重试 5 次；工具错误作为结果回喂不中断循环 |
| 后台任务 | `bash` / `task` 加 `background=true`，`/tasks` 查看状态，完成自动通知 |

`max_tool_rounds` 和 `subagent_max_rounds` 分别限制主 Agent 和子 Agent 实际执行工具的轮数，
默认均为 `"unlimited"`（不限制轮数），也可设置为非负整数，例如 `50`；`0` 表示禁止执行工具。
一轮可包含多个工具调用。达到数字上限后仍允许模型生成最终文本；
若模型继续请求工具，则报告达到上限，不执行额外工具，已完成的操作保留。

## 文件工具与系统提示词

默认提供四个文件与命令工具，主 Agent 和子 Agent 共用相同实现：

| 工具 | 参数和行为 |
|---|---|
| `read` | `path`，可选 `offset`（从 1 开始）和 `limit`；读取 UTF-8 文本，每页最多 2,000 行 / 32 KiB，返回继续读取的 offset；不支持图片或二进制文件 |
| `bash` | `command`，可选 `timeout` 和 `background`；执行命令、搜索文件和运行测试，保留后台任务支持 |
| `edit` | `path` 与 `edits: [{oldText, newText}]`；所有匹配都基于原文件，必须非空、唯一且互不重叠；全部验证通过才写入 |
| `write` | `path` 与 `content`；创建或完整覆盖文件，自动创建缺失的父目录 |

文件工具支持绝对路径、相对启动工作目录的路径及 `~/`。edit / write 通过临时文件替换目标，
避免写入失败时留下截断文件；保留已有文件的权限，并跟随已有符号链接修改目标文件。
工具仍走统一的 hooks、权限审批和 Plan 模式检查；Ask When Need 下 edit / write 默认需要审批。

系统提示词借鉴 Pi 的分段方式：简短角色说明后依次组合 `<tools>`、`<rules>`、
`<addendum>`、`<project_context>`、`<skills>`、`<agent_memory>`、`<todos>`、
`<subagent>`、`<plan_mode>` 和 `<cwd>`，空的可选部分省略。
工具摘要和使用规则来自工具目录，扩展及 MCP 工具也会列入；Plan 模式隐藏执行工具，
子 Agent 不包含 task。每次模型请求前刷新记忆与 Todo，工具产生的新状态在下一轮立即可见。
技能只注入名称、描述和文件位置，通过 skill 工具按需获取正文。

可在当前项目下创建以下文件（全局回退位置为 `<config dir>/koala/`）：

| 文件 | 用途 |
|---|---|
| `.koala/SYSTEM.md` | 替换默认角色、tools 和 rules 前缀；API 工具定义及后续上下文、模式约束仍保留 |
| `.koala/APPEND_SYSTEM.md` | 在默认或自定义前缀后追加指令 |

每种文件都是项目优先、全局回退，不合并两处同名文件；空白 SYSTEM.md 使用默认前缀。
项目上下文自动加载全局配置目录以及从文件系统根目录到启动工作目录的祖先指令。
每个目录按 `AGENTS.override.md`、`AGENTS.md`、`CLAUDE.md` 选择第一个存在的文件，
按全局、祖先、当前目录排列。文件指令在每次 Agent run 开始时重新读取，修改后下一次 run 生效。
现有 turn_start / before_model 扩展注入分别放入独立上下文段，保留原有扩展行为。

## MCP 服务器

koala 可作为 MCP 客户端，连接本地 **stdio** 和远程 **Streamable HTTP** 服务器，
发现并调用服务器提供的工具。使用官方 [Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)。
在 `config.toml` 中添加服务器，重启后生效：

```toml
[mcp.servers.files]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/absolute/path/to/files"]
startup_timeout_secs = 60
timeout_secs = 60
# 可选：工作目录及传给子进程的环境变量（继承其余环境变量）
# cwd = "/absolute/path/to/workspace"
# env = { MY_API_KEY = "your-key" }
# 仅对自己确认只读的工具显式授权，填服务器原始工具名：
# read_only_tools = ["read_file", "list_directory"]

[mcp.servers.remote]
url = "https://example.com/mcp" # 替换为实际服务地址
bearer_token_env = "MY_MCP_TOKEN"
# headers = { X-Project = "my-project" }
# enabled = false
```

每个服务器必须且只能配置 `command` 或 `url`。`command` 和 `args` 按 argv 执行，
不经过 shell；相对路径基于启动 koala 的工作目录（设置 `cwd` 后，子进程参数中的相对路径基于该目录）。
HTTP token 从指定环境变量读取；不支持自动 OAuth 登录或旧版独立 SSE 端点。
不要把示例远程地址直接作为实际配置。stdio 服务器 stderr 被丢弃，以免破坏终端界面；
需要排查服务器日志时可让服务器写入自己的日志文件。

工具名为 `mcp__服务器名__原始工具名`，例如 `mcp__files__read_file`。
服务器名和工具名需由 ASCII 字母、数字、下划线或连字符组成，完整工具名不超过 64 字节；
不兼容或重复的名字会在启动时报告。工具描述、输入 schema、结构化结果、内容块和 `isError`
均保留；当前模型工具结果通道为文本，图片等内容块保留为 JSON，不直接显示为图片。

MCP 工具沿用现有 hooks、四级权限和 Plan 模式。默认视作非只读工具，
服务器的 `readOnlyHint` 不会自动授予权限。只有 `read_only_tools` 中明确列出的工具
允许在 Plan 模式使用，并可在 Ask When Need 中自动执行；Normal 仍逐次审批。
`permissions.allow` / `deny` 使用带 `mcp__` 前缀的完整工具名。
错误拼写的 `read_only_tools` 会导致启动失败，避免静默改变权限。

启动时完成握手和分页工具发现，连接在主 Agent、子 Agent 和新会话之间复用。
默认启动超时为 30 秒，每次工具调用超时为 60 秒，均须为正整数。
已启用服务器连接失败时启动报错；临时停用可设置 `enabled = false`。
中断或超时会发送 MCP 取消通知，不自动重试工具调用；已在服务器完成的操作不会撤销，
服务器是否停止执行取决于其取消实现。断开连接后需重启 koala，工具列表变化也需重启加载。
当前集成范围为 MCP tools，不包含 resources、prompts、sampling 或 elicitation 的交互入口。

本地 MCP 服务器与 koala 拥有相同系统权限，配置服务器即允许启动该程序；
工具审批不限制服务器自身的启动行为。

## Extension 开发与安装

扩展是可信的本地程序，可读写本机文件、访问网络；它与 Agent 具有相同的系统权限。
工具权限控制只约束 Agent 发起的工具调用，不是扩展进程的沙箱。

安装仓库内的示例扩展：

```bash
koala extension-install examples/extensions/context
```

命令复制目录至 `.koala/extensions/<name>`，拒绝覆盖已有安装、符号链接和特殊文件。
可用 `--directory path/to/extensions` 指定安装目录；默认安装位置固定为 `.koala/extensions`，
不随 `KOALA_WORKSPACE` 改变。随后把命令输出的 manifest 路径加入配置（重启生效）：

```toml
[extensions]
manifests = [".koala/extensions/context-example/extension.toml"]
timeout_secs = 30
```

所有扩展（包括 memory）统一按 `manifests` 加载；删除对应路径即停用，
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
`command` 为 argv，不经过 shell 展开。通用环境变量 `KOALA_EXTENSION_CWD` 提供 Agent 启动目录。stdin 接收一个 JSON 对象，stdout 必须返回一个
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

所有扩展按配置顺序执行，后续扩展可看到前序改写的参数和结果。
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
`Extensions::register` 接入同一协议。Agent 不再导出 `koala::memory`；知识库库 API 属于独立的 `koala_memory` 包。

## 开发

```bash
cargo fmt --check                   # 格式检查
cargo test                          # 测试
cargo clippy --all-targets -- -D warnings
```


### 独立 memory 扩展的构建与迁移

依赖方向：Agent → 通用扩展运行时 → 版本化协议；memory → 版本化协议。
通用 LLM、Markdown 和进程模块位于独立基础包，均不依赖 Agent。
memory 不属于 Agent 的 Cargo workspace，不参与 Agent 的默认构建或测试。
私有 `remember` 便签仍属于 Agent 本身，与共享知识库扩展无关。

```bash
# 构建并安装独立命令（不编译 Agent/TUI/MCP）
cargo install --path extensions/memory
# 生成带可执行文件的自包含安装包；目录必须不存在
koala-memory package /tmp/koala-memory-package --memory-workspace "$PWD/.koala"
# 通用安装命令；无需 Agent 中有任何 memory 注册代码
koala extension-install /tmp/koala-memory-package
```

然后在 Agent 的 `[extensions]` 中设置：

```toml
[extensions]
manifests = [".koala/extensions/memory/extension.toml"]
timeout_secs = 30
```

重启后生效。安装包携带当前平台可执行文件，运行无需 Rust 工具链或源码。
修改知识库目录时，编辑 manifest 的 `--workspace` 参数即可；打包时相对目录会转为绝对目录，
因此复制安装包不会意外更换知识库。CLI 的 `--workspace` 优先于 `KOALA_WORKSPACE`。

原 `[memory]` 与 `extensions.memory` 不再是 Agent 配置项。已有知识库文件无需迁移；
安装时指定原 workspace。原 `koala search/read/distill/dream` 改为独立命令：

```bash
koala-memory --workspace .koala search "所有权"
koala-memory --workspace .koala read digest/wiki/ownership.md
koala-memory --config memory.toml distill .koala/session/SESSION.jsonl
koala-memory --config memory.toml dream
```

`distill` 必须显式传入转录文件路径，扩展不读取 Agent 内部会话配置。
`memory.toml` 由扩展独立读取，可包含 `[memory] workspace` 及 `[llm] base_url/api_key/model/headers`；
也可用 `--config config.toml` 显式读取旧配置完成迁移。扩展不会从 Agent 隐式接收 API key，
LLM 配置只在蒸馏/整合时需要。详见 [memory 扩展文档](extensions/memory/README.md)。

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --manifest-path extensions/memory/Cargo.toml
cargo clippy --manifest-path extensions/memory/Cargo.toml --all-targets -- -D warnings
```
