# Extension UI 协议与交互宿主设计

状态：用户已确认设计并授权实现；协议、宿主、TUI 和示例已实现，验证记录见实现计划。

## 目标与范围

用户要求在 Koala extension 中加入 UI 协议、宿主渲染和交互组件。首版提供通知、状态文本、输入框上下方的组件，以及按钮、单选列表、单行输入和确认弹窗。扩展可展示选择结果、接收用户输入，再返回新的界面状态。无需修改 Koala 源码即可使用这些能力。

首版不提供任意绘制回调、HTML、编辑器替换、多字段表单和扩展自定义全局快捷键。交互通过键盘完成。保留现有工具、权限和模型对话行为。

## 方案选择

采用声明式 JSON UI 和独立的 UI 事件回调。继续使用“启动扩展进程、stdin 请求、stdout 响应、进程退出”的调用模式。

另一种方案是常驻扩展进程与双向流式 RPC，可实时推送 UI，但会引入连接恢复、进程存活和并发请求管理。直接加载原生 UI 插件则会引入语言和 ABI 约束。首版选用声明式协议，适配已有运行时与 Rust/Ratatui 宿主。

## 版本与能力

- 保持 `api_version = 1` 原有请求与响应行为；新增能力使用 `api_version = 2`，宿主同时支持两版。
- v2 manifest 可声明 `ui = true`，允许会话初始化通知和用户交互回调。未声明的扩展不得发出 UI 指令。
- v2 请求携带 `session_id` 和 `capabilities.ui`。主 TUI 会话为 true；子 Agent、无 UI 调用和临时 `/btw` 对话不提供 UI 能力。
- UI 未启用时，不启动 UI 初始化回调；收到 UI 指令时忽略显示并不等待用户输入，普通工具结果仍有效。扩展通过能力字段自行提供文本回退。
- 原协议 crate 不依赖 Ratatui 或 Agent；UI 类型可序列化。Rust 扩展的新增 trait 方法提供默认实现，既有实现保持可用。

## 指令与组件

响应新增可选 `ui` 数组，默认为空。指令包括：

| 指令 | 语义 |
| --- | --- |
| `notify` | 显示带扩展名称的 info/warning/error 通知 |
| `set_status` / `remove_status` | 更新、移除带 ID 的单行状态文本 |
| `set_widget` / `remove_widget` | 整体替换、移除一个具名组件，位置为 above_editor 或 below_editor |
| `open_dialog` / `close_dialog` | 打开、替换或关闭扩展自己的交互弹窗 |

组件由顺序排列的 blocks 构成：`text`、`markdown`、`button`、`select`、`input`。每个交互 block 必须有稳定的 `id`；单选项也有独立 ID。input 有 label、初始 value；用户按 Enter 才提交，不对每次按键启动进程。确认弹窗采用 `kind: confirm`，返回布尔值或取消事件。

例：扩展在 hook 或工具响应中显示单选组件。

```json
{
  "ui": [{
    "type": "set_widget",
    "id": "documents",
    "placement": "above_editor",
    "blocks": [
      {"type": "text", "text": "选择要打开的文档"},
      {"type": "select", "id": "document", "label": "文档", "options": [
        {"id": "doc-1", "label": "安装说明"},
        {"id": "doc-2", "label": "开发指南"}
      ]}
    ]
  }]
}
```

扩展名称由宿主绑定，不接受响应伪造来源。所有 UI ID 均以会话与扩展名称隔离；同名扩展内的 widget 与 dialog 使用不同资源类型区分。

## 事件回传与状态

宿主为每次组件替换和交互完成生成进程内不重复的 revision，为每次操作生成 event_id。宿主快照另有仅在组件替换时变化的 content_revision，使确认回执和错误恢复不会清空未提交的输入草稿。主会话初始化时，对声明 UI 能力的扩展发送 `kind: ui_event, event.type: mount`。恢复或新建会话重新 mount；不从对话记录重放交互组件。

用户确认选择时发送如下请求，扩展仍通过原 manifest.command 启动：

```json
{
  "api_version": 2,
  "kind": "ui_event",
  "session_id": "host-session-id",
  "capabilities": {"ui": true},
  "event": {
    "type": "select",
    "event_id": "host-event-id",
    "surface": "widget",
    "surface_id": "documents",
    "revision": 3,
    "control_id": "document",
    "value": "doc-2"
  }
}
```

其他事件为 `click`、`submit`、`confirm`、`cancel`；不同事件值分别为无值、字符串、布尔值或无值。响应仍使用 `ui` 数组更新组件。`ui_event` 响应不允许修改模型上下文、工具参数或工具结果；此类字段返回协议错误，避免静默丢失。

UI 是异步交互：显示弹窗不会暂停正在运行的模型轮次。需要确认后执行的扩展动作应在 `confirm` 回调中执行。工具必须照常返回自己的 `content`，UI 操作不会自动提交新的用户消息。

每个会话拥有独立 UI 控制器和回调任务，不等待 Agent 的轮次互斥锁。每个扩展最多执行一个 UI 回调，允许最多 16 个待处理事件；超限拒绝并提示。宿主校验当前组件、revision、control ID 和选项值，消费 event_id 后不重复执行。同一组件提交后禁用操作，直至回调完成。

提交前校验的 revision 与完成时仍须一致。组件被其他 hook 更新后，旧回调的整批 UI 更新丢弃。回调不自动重试，避免重复执行扩展动作。回调期间可能与同一扩展的普通 hook 并发；扩展跨进程持久化状态需自行协调，并在文档中明确说明。

## 宿主布局与键盘操作

- 状态项放在独立扩展状态行；widget 放在输入框上下方，宿主统一布局和配色。
- 加入 `/extensions` 命令，打开组件列表，可聚焦某个交互组件；避免抢占输入框现有 Tab 补全和 Shift+Tab 模式切换。
- 聚焦组件后，Tab/Shift+Tab 切换控件；方向键选择；Enter 激活或提交；Esc 返回输入框。打开 widget 不抢焦点。
- 弹窗显示扩展名称；确认弹窗默认选择取消。Esc 关闭弹窗并发送一次 cancel 事件。
- 权限确认高于扩展弹窗；权限提示存在时扩展不得接收键盘或粘贴。全局取消/退出快捷键保留。
- 多个弹窗按到达顺序展示，同扩展同 ID 的更新替换已有条目。关闭内置面板前不强制覆盖其焦点。
- 长列表与文本可滚动，保留当前焦点可见；组件区域总高度限制在终端可用高度的三分之一，小尺寸下优先保留输入与权限提示。详情模式隐藏内联组件，可从组件面板查看。
- 文本统一清理终端控制字符；Markdown 使用现有宿主渲染器。组件输入草稿不写入聊天输入框。

## 生命周期与错误

- UI 状态仅属于当前主会话，不持久化到 LLM 历史、转录或记忆。可见通知仍属于当前 UI 展示。
- 取消、重置、恢复、分支导航和退出必须中止相关 UI 回调及其子进程，丢弃排队事件；会话切换清空全部 UI 状态。
- 普通取消保留静态组件，关闭弹窗并恢复输入焦点；取消不会再触发有副作用的取消回调。
- 超时沿用 extension timeout；回调报错显示带扩展名称的诊断，组件解除禁用，其他扩展与聊天继续工作。
- 消费者关闭后停止回调，不遗留等待。旧会话事件通过会话 generation 隔离，不能写入新会话。
- 单批最多 64 条 UI 指令；每扩展最多 32 个 widget、16 个状态项、8 个弹窗；单组件最多 64 blocks，单选最多 256 项，ID 最多 128 字节，单文本/输入值最多 16 KiB，单批 UI 数据最多 256 KiB。超限或非法结构整批拒绝，不做部分更新。
- hook 响应先验证整个响应，再提交 UI 更新。已有 block 或协议错误使该响应失败时，不留下半完成的 UI 状态。UI 事件错误不终止无关模型轮次。

## 代码边界

- `crates/extension-api`：v2 UI 指令、组件、事件、能力与验证；新增 UI 回调默认方法。
- `crates/extensions`：v1/v2 加载与请求构造、来源绑定、回调路由；保留无 Agent 依赖。
- `src/agent/extension_ui.rs`：新增会话级 UI 控制器，负责状态、revision、事件验证、限额和任务生命周期。
- `src/agent/event.rs`、`session.rs`：UI 命令/事件连接，以及主会话初始化、取消、切换、退出清理。
- `crates/extensions/src/ui.rs`：统一装饰已注册扩展，验证 hook/工具响应后投递带来源的 UI 指令；会话用任务局部上下文覆盖完整轮次与手动压缩，子 Agent 显式清除 UI 能力，避免在每个 hook 调用点重复接线。
- `src/tui/extension_ui.rs`：新增声明式渲染、组件焦点、输入草稿与滚动逻辑；通过现有 SessionCommand/UiEvent 边界通信。
- `src/tui/view.rs`、`controls.rs`、`input.rs`、`mod.rs` 和 i18n：布局、组件面板入口、键盘/粘贴路由、中文和英文提示。
- `examples/extensions/interactive` 与 README：无第三方依赖的 Python 示例，演示选择、输入、确认、按钮和组件更新，以及 v1/v2 兼容说明。

## 验收

1. 现有 v1 示例和扩展测试通过；v2 安装与加载、能力声明和非法响应有协议测试。
2. 在无外部模型服务的集成测试中完成 mount → 选择文档 → 回调 → 更新内容，以及输入 → 确认 → 按钮动作的完整链路。
3. 模型轮次运行时 UI 回调仍能完成；回调挂起时取消和新建会话立即生效，进程被回收。
4. 测试扩展来源隔离、无效选项、重复事件、旧 revision、旧会话结果及限额，确保不执行错误回调。
5. Ratatui TestBackend 验证两种 widget 位置、弹窗、键盘/粘贴、中文宽字符、长内容、权限焦点、小终端，以及聊天草稿保留。
6. 子 Agent、临时对话及无 UI 环境不等待用户交互，UI 内容不进入模型历史。
7. 运行格式检查、相关 crate 与 TUI 测试，再运行 workspace 测试；报告任何环境阻塞。
