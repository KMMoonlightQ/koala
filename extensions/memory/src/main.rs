use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use koala_extension_api::{Extension, Stage};
use koala_memory::{FileStore, MemoryExtension, llm::LlmClient};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "Independent file knowledge-base extension")]
struct Cli {
    /// Extension-owned configuration; also accepts legacy [memory]/[llm] sections.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Serve one versioned JSON request on stdin/stdout.
    Serve,
    Search {
        query: String,
        #[arg(short = 'k', long, default_value_t = 5)]
        limit: usize,
    },
    Read {
        path: String,
        #[arg(long)]
        start: Option<usize>,
        #[arg(long)]
        end: Option<usize>,
    },
    Distill {
        session: PathBuf,
    },
    Dream,
    /// Create a self-contained installable extension with this executable.
    Package {
        directory: PathBuf,
        #[arg(long)]
        memory_workspace: PathBuf,
    },
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct Settings {
    memory: MemorySettings,
    llm: LlmSettings,
}
#[derive(Deserialize)]
#[serde(default)]
struct MemorySettings {
    workspace: PathBuf,
}
impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            workspace: ".koala".into(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct LlmSettings {
    base_url: String,
    api_key: String,
    model: String,
    headers: std::collections::HashMap<String, String>,
}
impl Settings {
    fn load(cli: &Cli) -> Result<Self> {
        let mut cfg: Self = match &cli.config {
            Some(path) => toml::from_str(&std::fs::read_to_string(path)?)?,
            None => Self::default(),
        };
        if let Ok(path) = std::env::var("KOALA_WORKSPACE") {
            cfg.memory.workspace = path.into();
        }
        if let Some(path) = &cli.workspace {
            cfg.memory.workspace = path.clone();
        }
        // Process cwd is the package directory; user paths refer to the host cwd.
        let cwd = std::env::var_os("KOALA_EXTENSION_CWD")
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir()?);
        if cfg.memory.workspace.is_relative() {
            cfg.memory.workspace = cwd.join(&cfg.memory.workspace);
        }
        for (name, field) in [
            ("KOALA_BASE_URL", &mut cfg.llm.base_url),
            ("KOALA_API_KEY", &mut cfg.llm.api_key),
            ("KOALA_MODEL", &mut cfg.llm.model),
        ] {
            if let Ok(value) = std::env::var(name) {
                *field = value;
            }
        }
        Ok(cfg)
    }
    fn client(&self) -> Result<LlmClient> {
        if self.llm.model.is_empty() {
            bail!("configure llm.model in the extension config or KOALA_MODEL");
        }
        let base = if self.llm.base_url.is_empty() {
            "https://api.openai.com/v1"
        } else {
            &self.llm.base_url
        };
        Ok(LlmClient::new(
            base,
            &self.llm.api_key,
            &self.llm.model,
            &self.llm.headers,
        ))
    }
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Request {
    Hook {
        api_version: u32,
        stage: Stage,
        payload: Value,
    },
    Tool {
        api_version: u32,
        name: String,
        arguments: Value,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Settings::load(&cli)?;
    match cli.command {
        Command::Serve => {
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw)?;
            let extension = MemoryExtension::new(cfg.memory.workspace);
            let response = match serde_json::from_str::<Request>(&raw)? {
                Request::Hook {
                    api_version: 1,
                    stage,
                    payload,
                } => extension.hook(stage, &payload).await,
                Request::Tool {
                    api_version: 1,
                    name,
                    arguments,
                } => extension.execute(&name, &arguments).await,
                _ => bail!("unsupported extension API version"),
            }
            .map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Command::Search { query, limit } => {
            for hit in FileStore::open(&cfg.memory.workspace)?.search(&query, limit) {
                println!(
                    "{}:{}-{} ({:.3})\n{}\n",
                    hit.path, hit.start_line, hit.end_line, hit.score, hit.text
                );
            }
        }
        Command::Read { path, start, end } => println!(
            "{}",
            FileStore::open(&cfg.memory.workspace)?.read_lines(
                &path,
                start.unwrap_or(1),
                end.unwrap_or(usize::MAX)
            )?
        ),
        Command::Distill { session } => println!(
            "{}",
            koala_memory::distill_session(
                &cfg.client()?,
                &mut FileStore::open(&cfg.memory.workspace)?,
                &session
            )
            .await?
        ),
        Command::Dream => println!(
            "{}",
            koala_memory::dream(
                &cfg.client()?,
                &mut FileStore::open(&cfg.memory.workspace)?,
                5
            )
            .await?
            .render()
        ),
        Command::Package {
            directory,
            memory_workspace,
        } => package(&directory, &memory_workspace)?,
    }
    Ok(())
}
fn package(directory: &Path, workspace: &Path) -> Result<()> {
    let workspace = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir()?.join(workspace)
    };
    let tools: Vec<_> = koala_memory::tools::definitions()
        .into_iter()
        .map(|t| {
            json!({
                "name": t.function.name, "description": t.function.description,
                "parameters": t.function.parameters, "read_only": t.function.name != "memory_write"
            })
        })
        .collect();
    let executable = format!("koala-memory{}", std::env::consts::EXE_SUFFIX);
    let manifest = json!({"api_version": 1, "name": "memory", "command": [format!("./{executable}"), "--workspace", workspace, "serve"], "hooks": ["turn_start"], "tools": tools});
    let encoded = toml::to_string_pretty(&manifest)?;
    std::fs::create_dir(directory).context("package destination must not exist")?;
    let result = (|| -> Result<()> {
        std::fs::copy(std::env::current_exe()?, directory.join(executable))?;
        std::fs::write(directory.join("extension.toml"), encoded)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(directory);
    }
    result?;
    println!("{}", directory.join("extension.toml").display());
    Ok(())
}
