# Pi agent 的 system prompt 调研

调研日期：2026-09-18。对象为 Pi coding agent；原 `badlogic/pi-mono` 官方仓库现重定向到 `earendil-works/pi`。以下源码核对固定于 [commit 13784598d27491e70ddcb549859f722417001786](https://github.com/earendil-works/pi/tree/13784598d27491e70ddcb549859f722417001786)，该提交中 coding-agent 的 [package.json](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/package.json) 标记版本为 0.85.1。这是源码快照，不把它等同于 npm 最新发布版。

## 结论

Pi coding agent 的 system prompt 是动态构造的：默认编程助手说明、当前工具及其规则、Pi 自身文档指引，再叠加用户补充、项目上下文、skills 目录和工作目录。具体入口是 [system-prompt.ts](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/src/core/system-prompt.ts)。默认前缀之外，各部分使用 XML 标签组织为独立 section，便于会话中的增量更新。

## 默认内容与动态部分

默认前缀把模型定位为运行于 Pi harness 中的专业编程助手，职责包括读文件、执行命令、编辑代码和创建文件。`tools` 汇总有 prompt snippet 的当前工具；`rules` 根据工具组合及工具贡献的 guidelines 生成，并要求简洁回答、清楚显示文件路径；`docs` 指引仅在处理 Pi 自身问题时阅读安装位置中的官方文档和示例。[构造源码](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/src/core/system-prompt.ts)

普通构造顺序为 `preamble` → `tools` → `rules` → `docs` → 可选 `addendum` → 可选 `project_context` → 可选 `skills` → `cwd`。扩展还能提供额外命名 sections。`customPrompt` 会替换默认前缀及默认 tools/rules/docs，但其后的上下文仍会追加；`forceSystemPrompt` 才是完整精确覆盖。[构造源码](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/src/core/system-prompt.ts)

## 用户如何改变它

| 入口 | 实际行为 |
| --- | --- |
| `--system-prompt` 或 loader 的 `systemPrompt` | 显式输入优先；输入若是存在的文件路径则读取文件，否则按文本处理；对应上述 `customPrompt`。 |
| `.pi/SYSTEM.md` | 当前项目已受信任时优先采用，否则回退到 agentDir 的 `SYSTEM.md`，通常是 `~/.pi/agent/SYSTEM.md`。 |
| `--append-system-prompt` | 可重复；显式提供时使用这些输入，不再自动发现 APPEND 文件。 |
| `.pi/APPEND_SYSTEM.md` | 当前受信任项目优先，否则回退到全局同名文件；两个文件不会自动合并。 |

来源：[参数解析](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/src/cli/args.ts)、[resource-loader.ts](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/src/core/resource-loader.ts)。应注意“替换默认 system prompt”的文档表述并不等于去掉所有后续项目上下文。

## AGENTS、skills 与扩展

项目上下文先加载全局 agentDir，再按祖先目录到当前目录排列；每个目录按 `AGENTS.override.md`、`AGENTS.md`、`AGENTS.MD`、`CLAUDE.md`、`CLAUDE.MD` 的候选顺序选择首个可读文件。嵌套 linked worktree 还会避免重复加载其遮蔽的主仓库规则。`noContextFiles` 可以禁用这一步。[resource-loader.ts](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/src/core/resource-loader.ts)

Skills 默认只注入名称、描述、文件位置和按需读取指引，不把全部 SKILL.md 正文塞入 system prompt；标记 `disableModelInvocation` 的 skill 不在该目录中出现。只有当前工具包含 `read` 或 `bash` 时，构造器才追加 skills section。[skills.ts](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/src/core/skills.ts)、[system-prompt.ts](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/src/core/system-prompt.ts)

`before_agent_start` 扩展可修改 `event.systemPromptOptions`，后续 handler 会看到前面的修改；若返回 `systemPrompt`，runner 将它设置为 `forceSystemPrompt`。本轮运行的 options 在 agent run 结束后清除。强制文本投影到模型请求中，同时保留当前工具声明；因此它并不意味着关闭工具。[extensions/runner.ts](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/src/core/extensions/runner.ts)、[agent-session.ts](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/coding-agent/src/core/agent-session.ts)

## 与 pi-agent-core 的区别

`pi-agent-core` 是通用 agent 运行时，不自带上面那套编程助手人设。其 `Agent` 初始化从调用者的 `initialState.systemPrompt` 和 tools 构造初始 system message；如果传入的 messages 已以 system message 开头，则不重复插入。不要将 README 中的示例 prompt 当作运行时内置默认 prompt。[agent.ts](https://github.com/earendil-works/pi/blob/13784598d27491e70ddcb549859f722417001786/packages/agent/src/agent.ts)

因此，要复现某次 Pi 请求的真实提示词，需要同时记录源码版本、活动工具、用户自定义文本、项目上下文、skills 和扩展修改；单独复制默认前缀无法完整复现。
