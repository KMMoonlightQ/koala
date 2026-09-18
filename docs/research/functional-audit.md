# koala 功能完整性审查

日期：2026-09-18。对象：当前工作区（包含上一轮私有记忆改造）。审查主链路、上下文压缩、后台任务、持久化、hooks/权限和知识库扩展。不是穷尽式安全审计；不以代码长短、命名或缺少复杂架构作为问题依据。

确认了六项行为问题，均用临时目录或本机 mock 模型服务复现，未请求真实模型、未修改真实记忆。P1 表示应优先处理；P2 表示特定规模或输入下的正确性/可靠性问题。生产代码未因本轮审查改动。

后续处理：知识库扩展已移除，通用 extension 协议、运行时和安装入口保留；前四项已修复。
当前回归测试见 `tests/functional_regressions.rs`。下文及 `functional-audit-probes/` 保留为改造前的审查证据，
旧复现断言不适用于修复后的代码，知识库复现需在移除前的版本运行。

## 1. [P1] pre-hook 出错会跳过后续阻断器，但工具仍执行

位置：[react.rs:281](../../src/agent/react.rs#L281)、[hooks.rs:64](../../src/agent/hooks.rs#L64)。

`run_all` 遇到任意非成功结果立即返回；执行工具的调用者只处理 `Blocked`，忽略 `Failed`。因此第一个 hook 非零退出、启动失败或超时后，后续 hook 不运行，失败本身也没有被展示，工具继续走权限检查并可能执行。

复现：配置 `pre_tool_use = ["exit 1", "exit 2"]`，权限为 `NeverAsk`，让 mock 模型调用 `write` 写临时文件。结果 `write executed=true`。第二个明确返回阻断码的 hook 完全被跳过。普通权限审批仍生效，这不是绕过所有权限，但违反 hook 作为额外阻断手段的语义。

建议：明确 pre-hook 的失败策略。对执行错误默认中止该调用并展示原因，不能把“未检查”当作“通过”；即使允许某类非阻断 hook 失败，也不能跳过剩余阻断器。区分检查拒绝、执行故障和成功。

## 2. [P1] 自动压缩在整轮结束后才运行，无法保护长工具循环

位置：[mod.rs:414](../../src/agent/mod.rs#L414)、[react.rs:95](../../src/agent/react.rs#L95)。

`react::run` 不断累加 assistant/tool 消息并继续请求；只有返回最终回答后，`run_turn` 才检查压缩阈值。默认工具轮数又不限。真实 provider 若先返回 context-length 错误，本轮无法到达自动压缩的位置。子 Agent 直接运行 ReAct，也没有这段结束后的压缩。

复现：阈值采用默认的 40,000 字节，八次读取约 7KB 的文件；九次连续模型请求都是正常流式请求，最后一次的序列化 messages 已达 **60,977 字节**，随后才出现摘要请求。序列化 messages 字节与阈值估算口径不完全相同，但八条文件正文自身已超过阈值。

建议：把预算判断移到每次模型请求前，覆盖主 Agent 和子 Agent。预算纳入消息、system context、工具定义及响应预留；压缩时保持工具调用/结果完整配对。超过 provider 容量时应有明确的缩减/恢复路径，而不是只能等待本轮成功结束。

## 3. [P2] 后台任务结果每次全部回灌，没有总预算和按需读取

位置：[background.rs:70](../../src/agent/background.rs#L70)、[react.rs:67](../../src/agent/react.rs#L67)。

每次请求遍历当前会话全部任务，各自截取最多约 8KB 再拼接。已完成结果没有消费状态、归档或总量预算。它们在克隆请求之后另行附加，不在被压缩的普通历史中，所以手动 `/compact` 也无法缩小这部分。

复现：十个完成任务各保存 8,000 字节结果，连续调用 `result_context()`，每次都是相同的 **80,381 字节**。

建议：保留小型任务状态索引；新完成结果可通知一次；完整内容通过按任务 ID 的工具读取。对所有后台上下文设置统一预算，已处理结果保留在持久化层，不再每次常驻。

## 4. [P2] 工作日志反复追加完整快照，并在流式热路径同步刷盘

位置：[work.rs:172](../../src/agent/work.rs#L172)、[work.rs:163](../../src/agent/work.rs#L163)、[react.rs:106](../../src/agent/react.rs#L106)。

每次模型响应及工具结果后都把全部 `messages` 追加进 `.work`。随着上下文增长，一段未压缩的工作过程会有近似二次的重复存储开销。每个流式文本片段也走同步 `trace → append → sync_data`，在 async 响应路径进行磁盘 I/O。加载日志则一次读入全部字节并重放。

复现：连续 100 次追加一条 1,000 字符消息并保存快照，当前上下文 JSON **102,901 字节**，日志 **5,200,250 字节**（约 50.5 倍）。这是可重复的写放大数据；未对真实磁盘延迟做性能基准，不声称已测得 UI 卡顿。

建议：将增量事件与最新快照分开；定期 checkpoint/轮转，恢复从最近快照开始。文本片段合批写入，明确定义刷盘时机和可接受的崩溃丢失窗口。

## 5. [P2] distill 只看会话开头，尾部结论和用户纠正被静默丢弃

位置：[distill.rs:117](../../extensions/memory/src/distill.rs#L117)、[distill.rs:133](../../extensions/memory/src/distill.rs#L133)。

`distillation_text` 从头拼接，超过 12,000 字节后直接截断并停止；提示词没有告诉模型这只是前缀。会话通常在尾部收敛，因而早期假设可能被提炼成记忆，最后的纠正却不在输入里。

复现：前面放超过 12KB 的旧假设，尾部用户消息标记 `FINAL_CORRECTION`。投影输出恰为 12,000 字节，纠正标记完全缺失。

建议：分段抽取、合并并处理前后矛盾；来源覆盖范围应可见。若预算不够，显式记录未处理部分，不能把前缀摘要包装成整个会话的结论。仅改成截取尾部同样会遗漏前文约束。

## 6. [P2] dream 达到单元上限后，仍可能把整份来源标成已处理

位置：[dream.rs:149](../../extensions/memory/src/dream.rs#L149)、[dream.rs:117](../../extensions/memory/src/dream.rs#L117)。

抽取结果会 `.take(max_units)`；随后只要本轮保留下来的单元都写入成功，就保存整个来源文件的 fingerprint。没有“部分完成”状态，也无法区分“没有价值所以丢弃”和“因本轮上限暂未处理”。

复现：一张 daily 卡含独立主题 A/B，mock 返回两条单元，以 `max_units=1` 整合 A。再用 `max_units=10` 运行，返回 **changed=0**，B 未写入且不会再处理，除非改动来源文件。

建议：按单元/批次记录进度，抽取返回明确的覆盖完成标记和待处理游标；只有完整覆盖后才能提交文件 fingerprint。整合的操作预算不能隐式变成永久遗漏。

## 处理顺序

先修 pre-hook 的错误语义；再把上下文预算贯穿 ReAct、后台结果和子任务；然后修复 distill/dream 的覆盖与检查点；日志改为增量与周期快照。

这些问题的共同点是：主路径已打通，但失败状态、信息覆盖范围、预算或数据生命周期没有形成完整契约。现有 175 项测试通过，并不意味着这些未覆盖情形不存在。

## 复现材料

保留源码于 [agent.rs](functional-audit-probes/agent.rs) 和 [memory.rs](functional-audit-probes/memory.rs)。它们断言“当前问题存在”，不应当作为正常回归测试长期放进 tests；修复时应反转对应断言。

从仓库根目录执行以下命令可复现（需要允许本机回环端口）。临时目标文件名需确保未被其他工作占用：

```sh
cp docs/research/functional-audit-probes/agent.rs tests/audit_probe.rs
cargo test --test audit_probe -- --nocapture
cp docs/research/functional-audit-probes/memory.rs extensions/memory/tests/audit_probe.rs
cargo test --manifest-path extensions/memory/Cargo.toml --test audit_probe -- --nocapture
```

本次实际运行：核心四项复现通过，扩展两项复现通过；核心压缩场景又以默认 40,000 字节阈值单独确认。运行后生成的两个 `audit_probe.rs` 已移出 tests，生产代码和原有测试未修改。
