# CLI Agent 的跨会话记忆调研

调研日期：2026-09-18。对比 Claude Code、Gemini CLI、Codex CLI 的官方文档；Codex 另核对官方仓库 `main` 的记忆模块说明和提示词。下述是当日抓取内容，不代表本机安装版本，也未对各产品执行端到端实验。`main` 链接会变化。

## 结论

跨会话记忆本身不是问题；问题在于把未经筛选的工作流水账当成默认常驻上下文。“只保留精炼记忆”的方向成立，但需要区分**常驻的精炼入口**与**按需读取的详细记忆及证据**。只限制字数，不能解决错作用域、过期、重复、没有来源的问题。

三家的实现并不完全一致：Claude 强调项目内的短索引与主题文件；Gemini 提供分层记忆，并为实验性自动抽取增加候选审核；Codex 有独立的抽取、整合和检索层。不能把“必须用户说接着上次才能用记忆”或“任何项目进展都不能记”说成行业共识。以下产品事实给出对应一手来源，最后的改造建议属于针对 koala 的设计判断。

## 先区分三个概念

| 机制 | 目的 | 与新会话的关系 |
| --- | --- | --- |
| 指令文件，如 `CLAUDE.md`、`GEMINI.md`、`AGENTS.md` | 用户或团队维护行为约定 | 按配置和作用域加载；不等于系统自动学到的记忆 |
| 持久记忆 | 保存以后还值得参考的信息 | 可以跨会话使用，但应筛选、维护并保留适用范围 |
| 会话历史及压缩摘要 | 恢复或延续一段对话 | 服务于会话延续；不能因为内容已被压缩，就直接升级为长期记忆 |

例如 Claude 明确说明新会话没有此前对话历史，但可以加载 auto memory；上下文压缩则是在窗口接近上限时清理工具输出、概括对话，属于另一套机制。[Claude 工作原理](https://code.claude.com/docs/en/how-claude-code-works)

## Claude Code：项目记忆索引，详情按需加载

Auto memory 默认开启，按 Git 仓库划分目录 `~/.claude/projects/<project>/memory/`；同仓库 worktree 共享，支持配置覆盖。记忆类型为 `user`、`feedback`、`project`、`reference`。其中 `project` 允许保存无法从代码或 Git 推得的进行中工作、期限和决策；官方同时要求跳过可从代码推导的事实及 `CLAUDE.md` 已有内容，并非每次会话都必须写记忆。[官方记忆说明](https://code.claude.com/docs/en/memory#auto-memory)

`MEMORY.md` 是一条记忆一行的索引；新会话只自动加载前 200 行或 25KB，以先到者为准。详细主题文件用普通文件工具按需读取。接近上限时提醒压缩、合并或删除旧项；超限写入仍成功，但返回错误要求整理索引。可通过 `/memory` 管理；带 frontmatter 的记忆文件写入时记录 `modified` 时间。[加载及维护机制](https://code.claude.com/docs/en/memory#how-it-works)

**对 koala 的启发：** 截断应配合整理机制；常驻的是有组织的索引，而不是追加日志的前半部分。保留时效性背景并非绝对错误，缺少边界和更新才会使它误导后续任务。

## Gemini CLI：分层记忆，加可选的后台候选抽取

当前文档描述的是直接用 `write_file` / `replace` 编辑 Markdown 记忆：共享项目约定放仓库 `GEMINI.md`，私人项目笔记放项目私有记忆目录，跨项目偏好放 `~/.gemini/GEMINI.md`；要求简洁并避免不同层重复。不能继续把旧版专用 `save_memory` 工具当成当前完整方案。[Memory files](https://geminicli.com/docs/tools/memory/)

上下文分全局、工作区及按文件访问发现的 JIT 层，找到的相关内容拼入提示词；`/memory show` 可检查实际加载内容。它与 Claude 的“只常驻索引”不是相同设计；本次文档没有确立统一的记忆注入硬上限。[GEMINI.md 层级](https://geminicli.com/docs/cli/gemini-md/)

实验性 Auto Memory 默认关闭。启用后，启动时后台分析闲置至少三小时、至少十条用户消息的会话，生成持久事实或可复用流程候选；允许没有产出。结果保存在项目 inbox 的补丁或技能草稿中，用户通过 `/memory inbox` 接受后才生效，抽取器不能直接修改活动记忆。它还避免把大量工具输出原样复制进记忆。[Auto Memory](https://geminicli.com/docs/cli/auto-memory/)

完整历史另存 `~/.gemini/tmp/<project_hash>/chats/`，通过 `--resume` 等恢复；历史存储不等于记忆加载。[Session management](https://geminicli.com/docs/cli/session-management/)

**对 koala 的启发：** 明确记住的事实与系统从历史推断的候选，可以采用不同入库策略。人工审核是 Gemini 的一种选择，不是所有自动记忆的必要条件。

## Codex CLI：后台抽取、全局整合、分层检索

当前官方文档说明本地记忆默认关闭；开启后，`/memories` 可分别控制当前会话能否使用记忆、能否贡献生成输入。记忆位于 `~/.codex/memories/`，包含持久条目、摘要和证据；必需团队规则仍应放 `AGENTS.md`。[OpenAI 官方 Memories 文档](https://learn.chatgpt.com/docs/customization/memories)

官方仓库实现说明将流程分成两阶段：根会话启动后异步挑选符合条件的闲置历史，逐会话生成 `raw_memory` 和 `rollout_summary`；再统一整合到文件产物。无有用内容是合法成功结果。整合按使用次数和使用时间选择来源，并清理不再选中的摘要，不是无限追加。[记忆模块说明](https://github.com/openai/codex/blob/main/codex-rs/memories/README.md)

整合提示词规定：`memory_summary.md` 是常驻导航；`MEMORY.md` 是可检索的中间知识层；`rollout_summaries/` 保存详细证据。没有可复用收获就不更新；历史与工具输出当数据，不当指令；`applies_to` 保留工作目录边界。它还明确指出 `MEMORY.md` 不必极度短小——详情要有用，导航要精炼。因此，这是共享存储中的逻辑作用域，不能描述为每项目物理隔离。[整合提示词](https://github.com/openai/codex/blob/main/codex-rs/memories/write/templates/memories/consolidation.md)

配置参考还提供过期及处理预算：默认闲置六小时再纳入生成，考虑三十天内会话，每次启动最多十六个候选；长期未使用的记忆不再有资格参与整合。这不是承诺到期立即从所有成品文件删除。`AGENTS.md` 默认 32KiB 上限属于指令加载，不能误称为 auto memory 预算。[配置参考](https://learn.chatgpt.com/docs/config-file/config-reference)、[AGENTS.md](https://learn.chatgpt.com/docs/agent-configuration/agents-md)

## koala 改造前的问题

以下记录调研时的实现，后续改造已落地；当前行为、命令与迁移说明见 [README 的记忆章节](../../README.md#记忆)。源码链接指向工作区当前文件，旧行号仅供追溯调研上下文。

改造前，核心记忆是“私有便笺”的实现：

- **写入标准鼓励临时状态。** `remember` 的说明明确包含 working state、给未来自己的提示、临时偏好；模型保存完成记录符合工具当前定义。[remember.rs](/Users/sai/Documents/knownledge-base/kb-agent/src/agent/tools/remember.rs:15)
- **只有追加，没有维护接口。** 按天插入文本，`remember` 不提供条目更新、删除或去重。读取最多取文件开头 4000 **字节**，虽常量名为 `MAX_PROMPT_CHARS`，实际不是字符数。日志增长后新增内容可能一直落在截断部分。[agentmem.rs](/Users/sai/Documents/knownledge-base/kb-agent/src/agent/agentmem.rs:9)
- **默认用户级共享，加载不看相关性。** 默认路径是配置目录下 `koala/memory.md`，每次模型循环都重读并拼入 system prompt；新会话清空对话但不清空这份文件。[config.rs](/Users/sai/Documents/knownledge-base/kb-agent/src/config.rs:289)、[react.rs](/Users/sai/Documents/knownledge-base/kb-agent/src/agent/react.rs:30)、[mod.rs](/Users/sai/Documents/knownledge-base/kb-agent/src/agent/mod.rs:251)
- **另一套记忆扩展没有替核心便笺完成整理。** `extensions/memory` 有检索、独立 `distill` / `dream` 命令，但不会自动蒸馏或整合，更不等于核心 `remember` 已有此流程。[扩展说明](/Users/sai/Documents/knownledge-base/kb-agent/extensions/memory/README.md:1)

## 建议的最小改造方向

1. **先改入库标准。** 保存稳定偏好、明确纠正、以后会复用的约束及外部背景；默认排除完成播报、测试计数、单次运行输出、待办清单。没有新知识就不写。项目期限等例外要有作用域与失效条件。
2. **把日志与有效记忆分开。** 会话记录保留原始过程；记忆条目维护当前有效结论，并支持更新、合并、遗忘。对“163 个测试通过”保留在会话证据中；对“提交前需运行哪些检查”才考虑提炼成可复用约定。
3. **常驻内容采用小索引。** 全局仅放适用所有项目的偏好，项目记忆按项目匹配；详情按需检索，预算在选取、整理时控制。不要只把截断从头改成尾。
4. **记忆附上最小来源与新鲜度。** 至少记录类型、作用域、来源会话、更新时间；涉及代码状态再关联提交或验证时间。读取时将它视为历史证据，不能当作当前验证结果或当前用户授权。
5. **提供可见控制。** 支持查看本次加载内容、编辑或删除条目，并分别关闭读取与写入。是否加候选 inbox 可后续决定，首要收益来自选择性和可维护性。

验收应关注行为：新会话问候不自动播报旧完成状态；跨项目不混入项目记录；重复事实不重复新增；用户纠正能替换旧结论；超预算后重要新记忆仍可被检索；没有长期价值的会话不产生记忆。
