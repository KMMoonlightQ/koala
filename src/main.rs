use anyhow::Result;
use clap::{Parser, Subcommand};
use koala::agent::agentmem::{AgentMemory, Kind, Note, Scope};
use koala::{config, tui};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "koala", version, about = "Extensible terminal agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start an interactive chat session (default)
    Chat,
    /// Inspect and maintain curated private memory for the current project
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Install a local extension directory (contains extension.toml)
    ExtensionInstall {
        source: PathBuf,
        #[arg(long, default_value = ".koala/extensions")]
        directory: PathBuf,
    },
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// Show configuration and the index loaded into the next model request
    Show,
    /// List all visible records, including expired records (human inspection)
    List,
    /// Read one active record, including provenance and details
    Get {
        key: String,
        #[arg(long, value_enum, default_value = "project")]
        scope: Scope,
    },
    /// Search active records, including entries omitted from the prompt index
    Search { query: String },
    /// Create or replace a curated record; use the same key to correct it
    Set {
        key: String,
        #[arg(long, value_enum)]
        kind: Kind,
        #[arg(long, value_enum, default_value = "project")]
        scope: Scope,
        #[arg(long)]
        summary: String,
        #[arg(long, default_value = "")]
        details: String,
        #[arg(long)]
        expires_on: Option<String>,
    },
    /// Permanently remove one record in the selected scope
    Forget {
        key: String,
        #[arg(long, value_enum, default_value = "project")]
        scope: Scope,
    },
}

fn memory_command(command: MemoryCommand) -> Result<()> {
    let cfg = config::Config::load()?.agent;
    let memory = AgentMemory::new(
        cfg.memory_file,
        &std::env::current_dir()?,
        format!(
            "user:koala memory in {}",
            std::env::current_dir()?.display()
        ),
        cfg.memory_read,
        cfg.memory_write,
        cfg.memory_index_bytes,
    )?;
    if matches!(command, MemoryCommand::Show) {
        println!(
            "{}\n{}",
            serde_json::to_string_pretty(&memory.status())?,
            memory.content()?
        );
        return Ok(());
    }
    // An explicit human inspection remains available when model recall is off.
    memory.set_read_enabled(true);
    let value = match command {
        MemoryCommand::List => serde_json::json!(memory.entries(true)?),
        MemoryCommand::Get { key, scope } => serde_json::json!(memory.get(&key, scope)?),
        MemoryCommand::Search { query } => serde_json::json!(memory.search(&query)?),
        MemoryCommand::Set {
            key,
            kind,
            scope,
            summary,
            details,
            expires_on,
        } => serde_json::json!(memory.upsert(Note {
            key,
            kind,
            scope,
            summary,
            details,
            expires_on
        })?),
        MemoryCommand::Forget { key, scope } => {
            serde_json::json!({"deleted":memory.forget(&key, scope)?})
        }
        MemoryCommand::Show => unreachable!(),
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Chat) {
        Command::ExtensionInstall { source, directory } => {
            let manifest =
                koala::extensions::install(&source, &directory).map_err(anyhow::Error::msg)?;
            println!(
                "Installed. Add to [extensions].manifests in config.toml:\n{}",
                serde_json::to_string(&manifest)?
            );
        }
        Command::Memory { command } => memory_command(command)?,
        Command::Chat => tui::run(&config::Config::load()?).await?,
    }
    Ok(())
}
