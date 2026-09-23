# Rust 多模型 provider 库调研

调研日期：2026-09-23。目标是为 Koala 的 Rust `koala-llm` 选择模型接入层。版本与能力以文中链接的官方 crate 文档、发布包和项目源码为准；provider 的实际能力仍受所选模型和账号限制。

## 结论

**首选 `genai` 做 Koala 的 provider 层候选**。它是纯 Rust 的多 provider 客户端，支持原生 Anthropic、Gemini、Ollama、OpenAI Responses 等协议，也支持 OpenAI 兼容服务；流式事件区分文本、推理、工具调用和思维签名。它不内置 Koala 需要替换的 agent 循环，因此可以保留现有会话、工具执行和界面逻辑。[`genai` 0.6.5 文档](https://docs.rs/crate/genai/0.6.5)、[流式工具示例](https://docs.rs/crate/genai/0.6.5/source/examples/c21-tooluse-streaming.rs)

**备选 `rig-core`**。它提供更完整的统一消息和 provider 抽象，覆盖 20 多种 provider；自 0.41 起，agent 循环拆到 `rig-agent`，单用 `rig-core` 不必采用 Rig 的 agent 运行时。但其模型、消息、流事件类型较多，与 Koala 当前的 `Message` / `StreamDelta` 对接工作预计比 `genai` 大。这是依据两边公开 API 的集成成本判断，尚未做原型实测。[`rig-core` 0.42 文档](https://docs.rs/rig-core/latest/rig_core/)、[0.41 crate 拆分说明](https://github.com/0xPlaygrounds/rig/discussions/2225)

| 库 | 已确认的能力与范围 | 对 Koala 的取舍 |
| --- | --- | --- |
| [`genai` 0.6.5](https://docs.rs/crate/genai/0.6.5) | 官方列出 26 个 adapter，包括 OpenAI Chat/Responses、Anthropic、Gemini、Ollama、Bedrock、Vertex、DeepSeek、Groq、OpenRouter、阿里云、Moonshot、Z.ai 等；有流式文本/推理/工具调用、图片输入、工具结果、reasoning effort、模型映射与自定义 endpoint/auth。[adapter 列表](https://docs.rs/genai/latest/genai/adapter/enum.AdapterKind.html)、[内容类型](https://docs.rs/genai/latest/genai/chat/enum.ContentPart.html)、[选项](https://docs.rs/genai/latest/genai/chat/struct.ChatOptions.html)、[自定义连接](https://docs.rs/genai/latest/genai/struct.ClientBuilder.html) | 最贴近“只换 provider 层”。API key 可用环境变量或 `AuthResolver`；自定义网关通过 `ServiceTargetResolver`。MIT 或 Apache-2.0。[许可与版本](https://docs.rs/crate/genai/0.6.5/source/Cargo.toml) |
| [`rig-core` 0.42](https://docs.rs/rig-core/latest/rig_core/) | 官方列出 Anthropic、Azure OpenAI、ChatGPT、GitHub Copilot、DeepSeek、Gemini、Groq、Mistral、Ollama、OpenRouter、xAI 等；统一消息可表示图片、推理和工具调用，提供流式 completion、`base_url` 与 API key builder。[消息源码](https://docs.rs/rig-core/latest/src/rig_core/completion/message.rs.html)、[client builder](https://docs.rs/rig-core/latest/rig_core/client/struct.ClientBuilder.html) | 可单用 `rig-core` 保留 Koala agent 循环。Rig 另有 `rig-agent`，采用它才会引入另一套循环。Rig 0.42 为 MIT；该项目近期有较多破坏性升级，选用时宜锁定版本并做回归测试。[crate 拆分](https://docs.rs/crate/rig/latest/source/MIGRATING.md)、[许可](https://docs.rs/crate/rig/latest/source/Cargo.toml.orig) |
| [`llm-connector` 1.4](https://docs.rs/llm-connector/latest/llm_connector/) | 发布说明列出 12+ provider，支持 OpenAI、Anthropic、Google、Ollama、中国云服务等；提供通用流式输出、工具、图片/PDF、reasoning，以及请求级 API key/base URL 覆盖。[发布包说明](https://docs.rs/crate/llm-connector/latest/source/CHANGELOG.md)、[多模态类型](https://docs.rs/llm-connector/latest/llm_connector/types/message_block/index.html) | 也是 provider 层而非 agent 循环，MIT。注意 1.4 的 crate 主页安装示例仍写 `0.2`，采用时应按具体版本源码验证 API。[主页](https://docs.rs/llm-connector/latest/llm_connector/)、[许可](https://docs.rs/crate/llm-connector/latest/source/LICENSE) |
| [`async-openai` 0.42](https://docs.rs/async-openai/latest/async_openai/) | OpenAI 与 Azure OpenAI 客户端；支持 OpenAI Chat/Responses、SSE、工具/图片相关 API、自定义 `base_url`、API key 和 header。[配置](https://docs.rs/async-openai/latest/async_openai/config/struct.OpenAIConfig.html)、[Azure 配置](https://docs.rs/async-openai/latest/async_openai/config/struct.AzureConfig.html) | 适合只想强化现有 OpenAI 兼容接入；项目明确优先遵从 OpenAI 规范，不能替代 Anthropic/Gemini 等原生协议适配。MIT。[范围说明与许可](https://docs.rs/crate/async-openai/latest/source/README.md) |
| [`llm` 1.3.x](https://docs.rs/llm/latest/llm/) | 统一多个后端并带 CLI；公开 chat API 有流式块、工具、图片 MIME 与 reasoning effort。[chat API](https://docs.rs/llm/latest/llm/chat/index.html) | 功能比单纯 provider 接入更宽；默认启用 CLI/full 功能，若选择它需关闭默认 feature 并检查依赖与 API 适配。MIT。[Cargo 功能和许可](https://docs.rs/crate/llm/1.2.7/source/Cargo.toml.orig) |

## Koala 接入注意点

`koala-llm` 原先通过 `reqwest` 直接请求 `/chat/completions`，已有图片输入、`reasoning_content`、工具调用、SSE 聚合与 usage 类型。本次接入保留兼容端点原有路径，在 [provider 适配层](../../crates/llm/src/provider.rs)把这些类型映射到 `genai`，Koala 的 agent 循环继续沿用。验收重点是 **流式工具调用完成判定、推理内容、带图片的历史消息、usage、错误体和取消**；这些是多协议转换最容易出现差异的位置。[`genai` 流事件](https://docs.rs/crate/genai/0.6.5/source/examples/c21-tooluse-streaming.rs)、[Rig 流终态契约](https://docs.rs/rig-core/latest/rig_core/streaming/struct.StreamFinal.html)

`genai` 0.6.5 的 `Client::all_model_names()` 文档注释称非 Ollama adapter 使用静态列表，但该版本实际源码中 Anthropic、OpenAI、Gemini、DeepSeek 等 adapter 会请求服务端模型接口。并非所有 provider 都提供在线列表；Koala 在登录、启动和打开 `/model` 时尝试读取，失败时仍可用显式模型 ID。新模型若沿用现有协议，无需升级库即可尝试调用；新的协议或模型特有能力仍可能需要库更新。[模型列表方法](https://docs.rs/genai/0.6.5/genai/struct.Client.html#method.all_model_names)、[显式模型标识](https://docs.rs/genai/0.6.5/genai/enum.ModelSpec.html)

如果目标包括 Pi 风格的 **订阅登录/OAuth**，不能把“支持该模型 provider”直接理解成“支持其登录流程”。例如 `genai` 的 GitHub Copilot 示例要求 GitHub PAT（`models` scope）；Rig 0.42 则有单独的 ChatGPT OAuth 客户端和 Copilot 客户端。各 provider 的认证方式要逐一核实。[`genai` Copilot 示例](https://docs.rs/crate/genai/latest/source/examples/c99-github-copilot.rs)、[Rig ChatGPT OAuth 源码](https://docs.rs/rig-core/latest/src/rig_core/providers/chatgpt/mod.rs.html)、[Rig Copilot 文档](https://docs.rs/rig-core/latest/rig_core/providers/copilot/index.html)
