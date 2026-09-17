use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use kb_agent::llm::LlmClient;
use kb_agent::{config, memory, tui};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "kb-agent",
    version,
    about = "Chat agent with a file-based knowledge base"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start an interactive chat session (default)
    Chat,
    /// Search the knowledge base with BM25
    Search {
        query: String,
        #[arg(short = 'k', long, default_value_t = 5)]
        limit: usize,
    },
    /// Read a line range of a knowledge base file (1-based, inclusive)
    Read {
        path: String,
        #[arg(long)]
        start: Option<usize>,
        #[arg(long)]
        end: Option<usize>,
    },
    /// Distill a session transcript into a daily memory card.
    /// Defaults to the most recent file in the agent's session_dir.
    Distill { session: Option<PathBuf> },
    /// Consolidate changed daily cards into digests
    Dream,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Chat) {
        Command::Chat => tui::run(&config::Config::load()?).await?,
        Command::Distill { session } => {
            let cfg = config::Config::load()?;
            let llm = kb_llm(&cfg)?;
            let session = match session {
                Some(path) => path,
                None => latest_session(&cfg.agent.session_dir)?,
            };
            let mut store = memory::FileStore::open(&cfg.memory.workspace)?;
            let path = memory::distill_session(&llm, &mut store, &session).await?;
            println!("{path}");
        }
        Command::Dream => {
            let cfg = config::Config::load()?;
            let llm = kb_llm(&cfg)?;
            let mut store = memory::FileStore::open(&cfg.memory.workspace)?;
            let report = memory::dream(&llm, &mut store, 5).await?;
            println!("{}", report.render());
        }
        Command::Search { query, limit } => {
            let cfg = config::Config::load()?;
            let store = memory::FileStore::open(&cfg.memory.workspace)?;
            for hit in store.search(&query, limit) {
                println!(
                    "{}:{}-{} ({:.3})",
                    hit.path, hit.start_line, hit.end_line, hit.score
                );
                println!("{}\n", hit.text);
            }
        }
        Command::Read { path, start, end } => {
            let cfg = config::Config::load()?;
            let store = memory::FileStore::open(&cfg.memory.workspace)?;
            let text = store.read_lines(&path, start.unwrap_or(1), end.unwrap_or(usize::MAX))?;
            println!("{text}");
        }
    }
    Ok(())
}

fn kb_llm(cfg: &config::Config) -> Result<LlmClient> {
    if cfg.llm.model.is_empty() {
        bail!("llm.model is not configured");
    }
    Ok(LlmClient::new(
        &cfg.llm.base_url,
        &cfg.llm.api_key,
        &cfg.llm.model,
        &cfg.llm.headers,
    ))
}

/// Most recently modified *.jsonl in the session directory.
fn latest_session(dir: &std::path::Path) -> Result<PathBuf> {
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read session dir {}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl")
            && let Ok(mtime) = path.metadata().and_then(|m| m.modified())
            && best.as_ref().is_none_or(|(_, t)| mtime > *t)
        {
            best = Some((path, mtime));
        }
    }
    best.map(|(p, _)| p)
        .with_context(|| format!("no session files in {}", dir.display()))
}
