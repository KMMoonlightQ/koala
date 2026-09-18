use crate::extensions::memory::distill::slugify;
use crate::extensions::memory::{BUCKET_NAMES, Catalog, FileStore, MemoryError, tools};
use crate::llm::{LlmClient, LlmError, Message};
use serde::Deserialize;
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DreamError {
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error("extract step returned no parseable units")]
    ExtractFailed,
}

#[derive(Debug, Default)]
pub struct DreamReport {
    pub scanned: usize,
    pub changed: usize,
    pub integrated: Vec<String>,
    pub failed: Vec<String>,
}

impl DreamReport {
    pub fn render(&self) -> String {
        let mut out = format!(
            "scanned: {}\nchanged: {}\nintegrated: {}\nfailed: {}",
            self.scanned,
            self.changed,
            self.integrated.len(),
            self.failed.len()
        );
        for path in &self.integrated {
            out.push_str(&format!("\n+ {path}"));
        }
        for name in &self.failed {
            out.push_str(&format!("\n! {name}"));
        }
        out
    }
}

#[derive(Debug, Deserialize)]
struct Unit {
    name: String,
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    paths: Vec<String>,
}

const MAX_UNIT_ROUNDS: usize = 6;

/// Consolidate changed daily cards into digest nodes. Files whose units all
/// integrate successfully get checkpointed; failures stay pending for next run.
pub async fn dream(
    llm: &LlmClient,
    store: &mut FileStore,
    max_units: usize,
) -> Result<DreamReport, DreamError> {
    let catalog = Catalog::load(store.workspace())?;
    let daily_files = list_daily(store.workspace())?;
    let changed: Vec<(String, i64)> = daily_files
        .into_iter()
        .filter(|(rel, mtime)| catalog.checkpoints.get(rel).is_none_or(|cp| mtime > cp))
        .collect();

    let mut report = DreamReport {
        scanned: changed.len(),
        changed: changed.len(),
        ..Default::default()
    };
    if changed.is_empty() {
        return Ok(report);
    }

    let mut corpus = String::new();
    for (rel, _) in &changed {
        let text = store.read_lines(rel, 1, usize::MAX)?;
        corpus.push_str(&format!("--- 文件 {rel} ---\n{text}\n\n"));
    }

    let units = extract_units(llm, &corpus, max_units).await?;
    let mut catalog = catalog;
    let mut dirty = false;

    for unit in &units {
        match integrate_unit(llm, store, unit).await {
            Ok(path) => {
                report.integrated.push(path);
                for src in &unit.paths {
                    if let Some((_, mtime)) = changed.iter().find(|(rel, _)| rel == src) {
                        catalog.checkpoints.insert(src.clone(), *mtime);
                        dirty = true;
                    }
                }
            }
            Err(_) => report.failed.push(unit.name.clone()),
        }
    }
    if dirty {
        catalog.save(store.workspace())?;
    }
    Ok(report)
}

async fn extract_units(
    llm: &LlmClient,
    corpus: &str,
    max_units: usize,
) -> Result<Vec<Unit>, DreamError> {
    let prompt = format!(
        "以下是最近的 daily 记忆卡片。请抽取最多 {max_units} 个值得长期沉淀的记忆单元，输出 JSON 数组，每个元素：\n\
         {{\"name\": \"小写短横线 slug\", \"bucket\": \"personal|procedure|wiki\", \"summary\": \"可复用的内容摘要\", \"paths\": [\"来源 daily 路径\"]}}\n\
         - personal：用户偏好、个人事实\n- procedure：方法、流程、操作经验\n- wiki：通用知识、概念、决策先例\n\
         优先合并指向同一抽象的跨文件证据，丢弃短暂提及和没有复用价值的内容。只输出 JSON。\n\n{corpus}"
    );
    let reply = llm
        .chat(&[Message::user(prompt)], None)
        .await?
        .content
        .unwrap_or_default();
    let units = parse_units(&reply).ok_or(DreamError::ExtractFailed)?;
    Ok(units
        .into_iter()
        .filter(|u| !u.name.is_empty())
        .take(max_units)
        .collect())
}

fn parse_units(reply: &str) -> Option<Vec<Unit>> {
    let start = reply.find('[')?;
    let end = reply.rfind(']')?;
    serde_json::from_str(&reply[start..=end]).ok()
}

async fn integrate_unit(
    llm: &LlmClient,
    store: &mut FileStore,
    unit: &Unit,
) -> Result<String, DreamError> {
    let bucket = if BUCKET_NAMES.contains(&unit.bucket.as_str()) {
        unit.bucket.as_str()
    } else {
        "wiki"
    };
    let target = format!("digest/{bucket}/{}.md", slugify(&unit.name));
    let system = format!(
        "你是记忆整合器。把给定的记忆单元整合进长期记忆库 digest/。\n\
         目标文件：{target}\n\
         规则：\n\
         1. 先用 memory_search 查找相同或相关的已有 digest 节点。\n\
         2. 决定动作：CREATE（没有相同抽象则新建目标文件）/ CORROBORATE（同一记忆再次出现，向已有节点追加来源或强化表述）/ REFINE（补充边界、步骤、前提、细节）/ CORRECT（修正旧节点的错误或冲突）。\n\
         3. 写入的文件正文保留可复用抽象，## Sources 章节用完整句子以 wikilink 引用来源 daily 路径，并把相关 digest 节点织入正文。\n\
         4. 本单元恰好落到一个 digest 节点。完成后回复 done。"
    );
    let user = format!(
        "记忆单元：\nname: {}\nbucket: {}\nsummary: {}\n来源：{}",
        unit.name,
        bucket,
        unit.summary,
        unit.paths.join(", ")
    );
    let mut messages = vec![Message::system(system), Message::user(user)];
    let tool_defs = tools::definitions();
    for _ in 0..MAX_UNIT_ROUNDS {
        let reply = llm.chat(&messages, Some(&tool_defs)).await?;
        match &reply.tool_calls {
            Some(calls) if !calls.is_empty() => {
                messages.push(reply.clone());
                for call in calls {
                    let result = tools::dispatch(store, call);
                    messages.push(Message::tool(call.id.clone(), result));
                }
            }
            _ => return Ok(target),
        }
    }
    Ok(target)
}

/// All `daily/**/*.md` as (workspace-relative path, mtime unix secs).
fn list_daily(workspace: &Path) -> Result<Vec<(String, i64)>, DreamError> {
    let mut out = Vec::new();
    walk(workspace, &workspace.join("daily"), &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, i64)>) -> Result<(), DreamError> {
    if !dir.is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(dir).map_err(|source| MemoryError::Io {
        path: dir.display().to_string(),
        source,
    })?;
    for entry in entries {
        let path = entry
            .map_err(|source| MemoryError::Io {
                path: dir.display().to_string(),
                source,
            })?
            .path();
        if path.is_dir() {
            walk(root, &path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .map_err(|_| MemoryError::InvalidPath(path.display().to_string()))?;
            let mtime = path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            out.push((rel, mtime));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_units_lenient() {
        let reply = "结果如下：\n[{\"name\": \"rust-notes\", \"bucket\": \"wiki\", \"summary\": \"s\", \"paths\": [\"daily/2026-09-17/rust.md\"]}]\n完毕";
        let units = parse_units(reply).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].bucket, "wiki");
        assert!(parse_units("没有 json").is_none());
    }

    #[test]
    fn list_daily_collects_relative_paths() {
        let dir = std::env::temp_dir().join(format!("kb-agent-dream-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("daily/2026-09-17")).unwrap();
        fs::write(dir.join("daily/2026-09-17/a.md"), "x").unwrap();
        fs::write(dir.join("daily/2026-09-17.md"), "index").unwrap();
        fs::write(dir.join("daily/skip.txt"), "no").unwrap();
        let files = list_daily(&dir).unwrap();
        let rels: Vec<&str> = files.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(rels, vec!["daily/2026-09-17.md", "daily/2026-09-17/a.md"]);
        assert!(files.iter().all(|(_, m)| *m > 0));
        let _ = fs::remove_dir_all(&dir);
    }
}
