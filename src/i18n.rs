//! UI localisation: one flat key -> (English, Chinese) mapping table.
//!
//! Language is an explicit value threaded from the frontend (see `tui::App`)
//! and from the agent's shared state, never global mutable state, so tests stay
//! deterministic and two frontends could use different languages.
use serde::Deserialize;

/// Interface language. `/lang` saves a workspace preference; config.toml
/// supplies the initial default and KOALA_LANG overrides the startup value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    Zh,
}

impl Lang {
    pub const ALL: [Self; 2] = [Self::En, Self::Zh];

    /// Config file / `KOALA_LANG` spelling.
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Zh => "zh",
        }
    }

    /// Human-readable name, shown in `/lang` feedback.
    pub fn label(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Zh => "中文",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "en" | "en-us" | "en_us" | "english" => Some(Self::En),
            "zh" | "zh-cn" | "zh_cn" | "cn" | "chinese" | "中文" => Some(Self::Zh),
            _ => None,
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::En => Self::Zh,
            Self::Zh => Self::En,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::En => 0,
            Self::Zh => 1,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Zh,
            _ => Self::En,
        }
    }
}

/// Language shared with background work (sub-agents, tasks) that holds only a
/// clone of the agent's shared state. Frontends stay authoritative: they push
/// updates through `SessionCommand::SetLang`.
#[derive(Debug)]
pub struct LangCell(std::sync::atomic::AtomicU8);

impl LangCell {
    pub fn new(lang: Lang) -> Self {
        Self(std::sync::atomic::AtomicU8::new(lang.as_u8()))
    }

    pub fn get(&self) -> Lang {
        Lang::from_u8(self.0.load(std::sync::atomic::Ordering::Relaxed))
    }

    pub fn set(&self, lang: Lang) {
        self.0
            .store(lang.as_u8(), std::sync::atomic::Ordering::Relaxed);
    }
}

/// All keys in one place, so the enum and the exhaustiveness test can never
/// drift apart.
macro_rules! keys {
    ($($key:ident),* $(,)?) => {
        /// Every user-visible string. Keeping keys in one enum makes the table
        /// exhaustive at compile time: adding a key without translating it fails
        /// the build, and a typo can never fall back to a raw key at runtime.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Key {
            $($key),*
        }

        impl Key {
            pub const ALL: &'static [Self] = &[$(Self::$key),*];
        }
    };
}

keys!(
    InputPlaceholder,
    Current,
    NoOutput,
    HintSelectConfirmCancel,
    HintSelectResumeCancel,
    HintScrollBack,
    HintHistorySelect,
    HintTaskOutput,
    HintTasksSelect,
    CmdHelp,
    CmdSessions,
    CmdNew,
    CmdPlan,
    CmdModel,
    CmdPermissions,
    CmdEffort,
    CmdTasks,
    CmdTodos,
    CmdSkills,
    CmdCompact,
    CmdLang,
    CmdTheme,
    UsageTheme,
    InfoThemeSet,
    NoteThemeSaveFailed,
    CmdQuit,
    ProgressToolCall,
    ProgressToolResult,
    StatusGenerating,
    StatusRunningTool,
    StatusProcessing,
    StatusAwaitingApproval,
    StatusInterrupting,
    StatusContinuing,
    StatusCompacting,
    StatusNewSession,
    StatusRestoringSession,
    NoteHistoryLoadFailed,
    NoteHistorySaveFailed,
    NoteBusyDraftKept,
    NoteUnknownCommand,
    NoteNoModels,
    NoteNoReasoningEfforts,
    NoteToolInterrupted,
    NoteTaskNotStoppable,
    NoteBusyInterruptFirst,
    InfoCancelled,
    InfoSessionRestored,
    InfoNewSession,
    InfoAllowedOnce,
    InfoDeniedOnce,
    InfoPermissionSwitched,
    InfoModelSwitched,
    InfoEffortSwitched,
    InfoContextCompacted,
    InfoNothingToCompact,
    InfoLanguageSet,
    NoteLanguageSaveFailed,
    UsagePermissions,
    UsageModel,
    UsageEffort,
    UsageLang,
    PermNormalDesc,
    PermAskDesc,
    PermNeverDesc,
    TaskRunning,
    TaskStopping,
    TaskDone,
    TaskFailed,
    TaskStopped,
    ToolRunning,
    ToolSucceeded,
    ToolFailed,
    ToolCancelled,
    PanelSessions,
    PanelPermissions,
    PanelModel,
    PanelEffort,
    PanelTheme,
    ThemeAuto,
    ThemeLight,
    ThemeDark,
    PanelTodos,
    PanelHelp,
    PanelHistory,
    PanelTaskOutput,
    PanelTasks,
    PanelCommands,
    LoadingSessions,
    NoSessions,
    HistoryCount,
    NoHistoryMatches,
    NoTasks,
    TaskOutputRunning,
    TaskOutputStopped,
    MoreTodos,
    HelpText,
    DetailedLog,
    ActionDenyOrInterrupt,
    ActionClosePanelToInterrupt,
    ActionInterrupt,
    StatusBarBackground,
    StatusBarHintBack,
    StatusBarHintNewContent,
    StatusBarHintBottom,
    StatusBarHintHelp,
    PermissionTitle,
    AllowBadge,
    DenyBadge,
    PermissionFooter,
    ViewFullArgs,
    ArgsLabel,
    OutputLabel,
    MoreLines,
    RecordTitle,
    ErrModelNotConfigured,
    ErrUnsupportedEffort,
    InterruptNotice,
    UntitledSession,
    UnreadableSession,
    SessionInvalidId,
    SessionReadFailed,
    SessionNotAFile,
    SessionLineCorrupt,
    SessionLineInvalidRole,
    SessionNoRecords,
    SessionDirReadFailed,
    SessionDirEntryFailed,
    SystemBase,
    SystemPlan,
    SkillsSection,
    SubagentSystem,
);

/// The mapping table. Placeholders are `{name}`; fill them with [`fill`].
pub fn text(lang: Lang, key: Key) -> &'static str {
    use Key::*;
    let (en, zh) = match key {
        InputPlaceholder => ("Type a message…", "输入消息…"),
        Current => (" (current)", "（当前）"),
        NoOutput => ("(no output)", "（无输出）"),
        HintSelectConfirmCancel => (
            "↑↓ select · Enter confirm · Esc cancel",
            "↑↓ 选择 · Enter 确认 · Esc 取消",
        ),
        HintSelectResumeCancel => (
            "↑↓ select · Enter resume · Esc cancel",
            "↑↓ 选择 · Enter 恢复 · Esc 取消",
        ),
        HintScrollBack => (
            "↑↓ / PgUp/PgDn scroll · Esc back",
            "↑↓ / PgUp/PgDn 滚动 · Esc 返回",
        ),
        HintHistorySelect => (
            "↑↓ select · Enter/Tab insert · Esc cancel",
            "↑↓ 选择 · Enter/Tab 回填 · Esc 取消",
        ),
        HintTaskOutput => (
            "PgUp/PgDn scroll · x stop · Esc back to list",
            "PgUp/PgDn 滚动 · x 停止 · Esc 返回列表",
        ),
        HintTasksSelect => (
            "↑↓ select · Enter view output · x stop · Esc back",
            "↑↓ 选择 · Enter 查看输出 · x 停止 · Esc 返回",
        ),

        CmdHelp => ("Show keybindings and commands", "查看快捷键与命令"),
        CmdSessions => (
            "List saved sessions; Enter resumes the conversation",
            "查看历史会话，Enter 恢复并继续对话",
        ),
        CmdNew => (
            "Interrupt the current work and start a new session",
            "中断前台工作并开始新会话",
        ),
        CmdPlan => ("Toggle Normal / Plan mode", "切换 Normal / Plan 模式"),
        CmdModel => ("Open the model picker", "打开模型选择列表"),
        CmdPermissions => (
            "Choose Normal / Ask When Need / Never Ask",
            "选择 Normal / Ask When Need / Never Ask 权限",
        ),
        CmdEffort => ("Open the reasoning effort picker", "打开思考档位选择列表"),
        CmdTasks => ("View and stop background tasks", "查看、停止后台任务"),
        CmdTodos => ("Open the full todo list", "打开完整 Todo 列表"),
        CmdSkills => ("List loaded skills", "列出已加载 skills"),
        CmdCompact => ("Compact the conversation context", "压缩对话上下文"),
        CmdTheme => (
            "Set the theme (auto / light / dark)",
            "设置主题（auto / light / dark）",
        ),
        UsageTheme => (
            "Usage: /theme [auto|light|dark]",
            "用法：/theme [auto|light|dark]",
        ),
        InfoThemeSet => (
            "Theme: {theme} (auto / light / dark)",
            "主题：{theme}（auto / light / dark）",
        ),
        NoteThemeSaveFailed => (
            "Theme changed for this session, but could not save the preference: {e}",
            "本次会话的主题已切换，但无法保存主题偏好：{e}",
        ),
        CmdLang => ("Switch the interface language", "切换界面语言"),
        CmdQuit => ("Quit", "退出"),

        ProgressToolCall => ("\nTool {name}({summary})\n", "\n工具 {name}({summary})\n"),
        ProgressToolResult => ("\nResult: {output}\n", "\n结果：{output}\n"),
        StatusGenerating => ("Generating", "正在生成"),
        StatusRunningTool => ("Running {name}", "正在执行 {name}"),
        StatusProcessing => ("Processing results", "正在处理结果"),
        StatusAwaitingApproval => ("Waiting for approval", "等待权限确认"),
        StatusInterrupting => ("Interrupting", "正在中断"),
        StatusContinuing => ("Continuing", "正在继续"),
        StatusCompacting => ("Compacting context", "压缩上下文中"),
        StatusNewSession => ("Creating a new session", "正在创建新会话"),
        StatusRestoringSession => ("Restoring session", "正在恢复会话"),
        NoteHistoryLoadFailed => ("Failed to read input history: {e}", "输入历史读取失败：{e}"),
        NoteHistorySaveFailed => ("Failed to save input history: {e}", "输入历史保存失败：{e}"),
        NoteBusyDraftKept => (
            "Still running; your draft is kept — press Esc to interrupt and send",
            "正在执行，草稿已保留；Esc 中断后可发送",
        ),
        NoteUnknownCommand => ("Unknown command: /{cmd}", "未知命令：/{cmd}"),
        NoteNoModels => (
            "No selectable models; configure llm.models",
            "没有可选模型，请配置 llm.models",
        ),
        NoteNoReasoningEfforts => (
            "No reasoning efforts configured for this model (llm.reasoning_efforts)",
            "当前模型未配置思考档位（llm.reasoning_efforts）",
        ),
        NoteToolInterrupted => (
            "Interrupted (completed operations were not rolled back)",
            "已中断（已完成的操作不会撤销）",
        ),
        NoteTaskNotStoppable => (
            "Task #{id} already finished or cannot be stopped",
            "任务 #{id} 已结束或不可停止",
        ),
        NoteBusyInterruptFirst => (
            "A turn is running; press Esc to interrupt first",
            "正在执行，请先按 Esc 中断",
        ),
        InfoCancelled => (
            "Interrupted; you can keep typing. Background tasks keep running.",
            "已中断，可以继续输入。后台任务仍会继续运行。",
        ),
        InfoSessionRestored => (
            "Session {id} restored; you can continue",
            "已恢复会话 {id}，可继续对话",
        ),
        InfoNewSession => ("New session started", "新会话已开始"),
        InfoAllowedOnce => ("Allowed once", "已允许本次操作"),
        InfoDeniedOnce => ("Denied", "已拒绝本次操作"),
        InfoPermissionSwitched => (
            "Permission mode: {mode} — {description}",
            "权限已切换为 {mode}：{description}",
        ),
        InfoModelSwitched => ("Model switched to {name}", "模型已切换为 {name}"),
        InfoEffortSwitched => (
            "Reasoning effort switched to {effort}",
            "思考档位已切换为 {effort}",
        ),
        InfoContextCompacted => ("Context compacted", "上下文已压缩"),
        InfoNothingToCompact => ("Nothing to compact yet", "暂无需要压缩的内容"),
        NoteLanguageSaveFailed => (
            "Language changed for this session, but could not save the preference: {e}",
            "本次会话的语言已切换，但无法保存语言偏好：{e}",
        ),
        InfoLanguageSet => ("Language: {label}", "语言：{label}"),
        UsagePermissions => (
            "Usage: /permissions [normal|ask_when_need|never_ask]",
            "用法：/permissions [normal|ask_when_need|never_ask]",
        ),
        UsageModel => ("Usage: /model [model name]", "用法：/model [模型名]"),
        UsageEffort => ("Usage: /effort [level]", "用法：/effort [档位]"),
        UsageLang => ("Usage: /lang [en|zh]", "用法：/lang [en|zh]"),

        PermNormalDesc => ("Every tool call needs approval", "所有工具调用都需审批"),
        PermAskDesc => (
            "Risky or undecidable actions need approval",
            "危险或无法判定的操作需审批",
        ),
        PermNeverDesc => ("All tool calls run automatically", "所有工具调用自动执行"),

        TaskRunning => ("running", "进行中"),
        TaskStopping => ("stopping", "停止中"),
        TaskDone => ("done", "已完成"),
        TaskFailed => ("failed", "失败"),
        TaskStopped => ("stopped", "已停止"),

        ToolRunning => ("running", "进行中"),
        ToolSucceeded => ("succeeded", "成功"),
        ToolFailed => ("failed", "失败"),
        ToolCancelled => ("cancelled", "已中断"),

        PanelSessions => ("Sessions", "历史会话"),
        PanelPermissions => ("Permission Level", "权限等级"),
        PanelModel => ("Select Model", "选择模型"),
        PanelTheme => ("Select Theme", "选择主题"),
        ThemeAuto => ("Follow terminal", "跟随终端"),
        ThemeLight => ("Light appearance", "浅色主题"),
        ThemeDark => ("Dark appearance", "深色主题"),
        PanelEffort => ("Reasoning Effort", "思考档位"),
        PanelTodos => ("Todos", "Todo 列表"),
        PanelHelp => ("Help", "帮助"),
        PanelHistory => ("Search Input History", "搜索输入历史"),
        PanelTaskOutput => ("Task Output", "任务输出"),
        PanelTasks => ("Background Tasks", "后台任务"),
        PanelCommands => ("Commands", "命令"),
        LoadingSessions => ("Loading sessions…", "正在读取会话…"),
        NoSessions => ("No saved sessions", "暂无已保存的会话"),
        HistoryCount => ("{n} matches", "{n} 条"),
        NoHistoryMatches => ("No matching history", "没有匹配的输入历史"),
        NoTasks => ("No background tasks", "暂无后台任务"),
        TaskOutputRunning => (
            "The task is still running; full output appears when it finishes.",
            "任务执行中，完成后可查看完整输出。",
        ),
        TaskOutputStopped => (
            "The task was stopped and collected no output.",
            "任务已停止，没有已收集的输出。",
        ),
        MoreTodos => (
            "  … {n} more · Ctrl+T to see all",
            "  … 另 {n} 项 · Ctrl+T 查看完整列表",
        ),
        HelpText => (
            "Input & navigation\n\
             Enter sends · Shift+Enter / Ctrl+J / \\+Enter adds a newline\n\
             ↑↓ move within multi-line input, then recall history at the edges\n\
             Ctrl+R searches history; Enter/Tab inserts, Esc keeps your draft\n\
             Pasting multi-line text becomes one draft and is not sent\n\
             \n\
             Commands & panels\n\
             / filters commands · ↑↓ select · Tab complete · Enter run\n\
             Shift+Tab toggles Normal / Plan (when idle)\n\
             Ctrl+T opens/closes the full todo list · Ctrl+O shows details\n\
             /tasks shows background tasks: ↑↓ select · Enter output · x stop\n\
             ? (empty input) or /help opens this help\n\
             \n\
             Running & quitting\n\
             Esc closes a panel; with none open it interrupts the turn\n\
             Ctrl+C interrupts the turn; clears the input when idle\n\
             Approval prompt: arrows select · Enter confirms · Esc denies\n\
             Ctrl+D (empty input) or /quit exits\n\
             PgUp/PgDn scroll history · Ctrl+End jumps to the bottom\n\
             \n\
             Commands",
            "输入与导航\n\
             Enter 发送 · Shift+Enter / Ctrl+J / \\+Enter 换行\n\
             ↑↓ 在多行中移动，到首尾后召回历史\n\
             Ctrl+R 搜索历史，Enter/Tab 回填，Esc 保留原草稿\n\
             粘贴多行作为一段草稿，不会自动发送\n\
             \n\
             命令与面板\n\
             / 筛选命令 · ↑↓ 选择 · Tab 补全 · Enter 执行\n\
             Shift+Tab 切换 Normal / Plan（空闲时）\n\
             Ctrl+T 打开/关闭完整 Todo 列表 · Ctrl+O 详细记录\n\
             /tasks 查看后台任务：↑↓ 选择 · Enter 输出 · x 停止\n\
             ?（空输入）或 /help 打开帮助\n\
             \n\
             运行与退出\n\
             Esc 关闭面板；无面板时中断前台工作\n\
             Ctrl+C 中断前台工作；空闲时清空输入\n\
             权限确认：方向键选择 · Enter 确认 · Esc 拒绝\n\
             Ctrl+D（空输入）或 /quit 退出\n\
             PgUp/PgDn 历史 · Ctrl+End 回到底部\n\
             \n\
             命令列表",
        ),

        DetailedLog => ("Detailed log", "详细记录"),
        ActionDenyOrInterrupt => ("Esc deny · Ctrl+C interrupt", "Esc 拒绝 · Ctrl+C 中断"),
        ActionClosePanelToInterrupt => ("Close the panel to interrupt", "关闭面板后可中断"),
        ActionInterrupt => ("Esc interrupt", "Esc 中断"),
        StatusBarBackground => ("bg {n} · ", "后台 {n} · "),
        StatusBarHintBack => ("Esc back", "Esc 返回"),
        StatusBarHintNewContent => ("● new content · Ctrl+End", "● 新内容 · Ctrl+End"),
        StatusBarHintBottom => ("Ctrl+End to the bottom", "Ctrl+End 回到底部"),
        StatusBarHintHelp => ("? help", "? 帮助"),
        PermissionTitle => (" approval · this once ", " 权限确认 · 仅本次 "),
        AllowBadge => (" allow ", " 允许 "),
        DenyBadge => (" deny ", " 拒绝 "),
        PermissionFooter => (
            "Arrows select · Enter confirm · Esc deny · PgUp/PgDn view arguments",
            "方向键选择 · Enter 确认 · Esc 拒绝 · PgUp/PgDn 查看参数",
        ),
        ViewFullArgs => (
            "  … Ctrl+O to view full arguments",
            "  … Ctrl+O 查看完整参数",
        ),
        ArgsLabel => ("  arguments", "  参数"),
        OutputLabel => ("  output", "  输出"),
        MoreLines => (
            "    … {n} more lines · Ctrl+O expands",
            "    … 另 {n} 行 · Ctrl+O 展开",
        ),
        RecordTitle => ("Record {index}", "记录 {index}"),

        ErrModelNotConfigured => ("Unconfigured model: {name}", "未配置的模型：{name}"),
        ErrUnsupportedEffort => (
            "Unsupported reasoning effort: {requested}; available: {available}",
            "不支持的思考档位：{requested}；可选：{available}",
        ),
        InterruptNotice => (
            "[This turn was interrupted. Completed operations were not rolled back; \
             tools without a result may still have taken effect, so verify before continuing.]",
            "[本轮已中断。已完成的操作不会撤销；无结果的工具可能已产生副作用，请先核实再继续。]",
        ),
        UntitledSession => ("Untitled session", "未命名会话"),
        UnreadableSession => (
            "Unreadable session (Enter to see why)",
            "无法读取的会话（Enter 查看原因）",
        ),
        SessionInvalidId => ("Invalid session id", "无效的会话 ID"),
        SessionReadFailed => ("Cannot read session {id}: {e}", "无法读取会话 {id}：{e}"),
        SessionNotAFile => (
            "Session {id} is not a regular file",
            "会话 {id} 不是普通文件",
        ),
        SessionLineCorrupt => (
            "Session {id} line {line} is corrupt: {e}",
            "会话 {id} 第 {line} 行损坏：{e}",
        ),
        SessionLineInvalidRole => (
            "Session {id} line {line} has an invalid role",
            "会话 {id} 第 {line} 行角色无效",
        ),
        SessionNoRecords => (
            "Session {id} has no conversation records",
            "会话 {id} 没有对话记录",
        ),
        SessionDirReadFailed => (
            "Cannot read the session directory: {e}",
            "无法读取会话目录：{e}",
        ),
        SessionDirEntryFailed => (
            "Cannot read a session directory entry: {e}",
            "无法读取会话目录项：{e}",
        ),

        SystemBase => (
            "You are a personal assistant agent with long-term memory and execution \
             ability, running in the user's terminal.\n\
             How you work:\n\
             - `remember` is your private memory file; use it for user preferences, facts \
             and working state worth keeping.\n\
             - Break multi-step tasks down with todo_write and keep the progress updated.\n\
             - Use bash to operate on the local environment; hand self-contained subtasks \
             to a sub-agent with task.\n\
             - Answer concisely and directly, in the user's language.",
            "你是一个带长期记忆和执行能力的个人助手 Agent，运行在用户的终端里。\n\
             工作方式：\n\
             - remember 是你的私人记忆文件，用来记值得保留的用户偏好、事实和工作状态。\n\
             - 多步骤任务先用 todo_write 拆解并持续更新进度。\n\
             - 需要操作本地环境时用 bash；独立的子任务用 task 派给子 Agent。\n\
             - 回答简洁直接，用用户的语言。",
        ),
        SystemPlan => (
            "You are in plan mode: investigate and plan only (todo_write / skill / task); \
             do not modify any file or state. Write the plan into the todo list and wait for \
             the user to leave plan mode before executing.",
            "当前处于 plan mode：只做调查和规划（todo_write / skill / task），\
             不要修改任何文件或状态。把计划写进 todo 列表，等用户退出 plan mode 后再执行。",
        ),
        SkillsSection => (
            "Available skills; load the full text with the skill tool when needed:\n{listing}",
            "可用 skill，需要时用 skill 工具加载全文：\n{listing}",
        ),
        SubagentSystem => (
            "You are a sub-agent completing the subtask you were given, on your own. \
             You may use bash to operate on the environment. Return the result directly \
             without explaining your process.",
            "你是子 Agent，独立完成交给你的子任务。可以用 bash 操作环境。\
             完成后直接给出结果，不要解释过程。",
        ),
    };
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// [`text`] with `{name}` placeholders substituted.
pub fn fill(lang: Lang, key: Key, args: &[(&str, &str)]) -> String {
    let mut out = text(lang, key).to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_both_languages_and_matching_placeholders() {
        for &key in Key::ALL {
            let en = text(Lang::En, key);
            let zh = text(Lang::Zh, key);
            assert!(!en.is_empty() && !zh.is_empty(), "{key:?} is untranslated");
            assert_eq!(
                placeholders(en),
                placeholders(zh),
                "{key:?} placeholders differ"
            );
        }
    }

    #[test]
    fn language_codes_round_trip_and_toggle() {
        for lang in Lang::ALL {
            assert_eq!(Lang::parse(lang.code()), Some(lang));
            assert_eq!(lang.toggled().toggled(), lang);
            assert_ne!(lang.toggled(), lang);
        }
        assert_eq!(Lang::default(), Lang::En);
        assert_eq!(Lang::parse("ZH-CN"), Some(Lang::Zh));
        assert_eq!(Lang::parse("klingon"), None);
    }

    #[test]
    fn multiline_prompts_keep_no_continuation_artifacts() {
        for lang in Lang::ALL {
            for key in [
                Key::SystemBase,
                Key::SystemPlan,
                Key::SubagentSystem,
                Key::HelpText,
            ] {
                let text = text(lang, key);
                assert!(
                    !text.contains("\n ") && !text.contains("  "),
                    "{lang:?} {key:?} kept indentation from a line continuation: {text:?}"
                );
            }
        }
    }

    #[test]
    fn fill_substitutes_named_placeholders() {
        assert_eq!(
            fill(Lang::En, Key::StatusRunningTool, &[("name", "bash")]),
            "Running bash"
        );
        assert_eq!(
            fill(Lang::Zh, Key::StatusRunningTool, &[("name", "bash")]),
            "正在执行 bash"
        );
    }

    fn placeholders(s: &str) -> Vec<&str> {
        let mut found = Vec::new();
        let mut rest = s;
        while let Some(start) = rest.find('{') {
            let Some(end) = rest[start..].find('}') else {
                break;
            };
            found.push(&rest[start..start + end + 1]);
            rest = &rest[start + end + 1..];
        }
        found.sort_unstable();
        found
    }
}
