#[cfg(feature = "http")]
mod http;
mod out;
mod stdio;

use clap::{Parser, Subcommand};
use pdf_kernel::{AskOptions, ChatMessage, Engine, SummaryStyle};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Parser)]
#[command(name = "pdfk", version, about = "Summarize and question PDFs with Groq, Gemini or LM Studio")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summarize a PDF
    Summarize {
        file: PathBuf,
        /// brief, detailed or bullets
        #[arg(short, long, default_value = "detailed")]
        style: SummaryStyle,
        /// Use only this provider (groq, gemini, lmstudio)
        #[arg(short, long)]
        provider: Option<String>,
        /// Print the full JSON result
        #[arg(long)]
        json: bool,
    },
    /// Ask one question about a PDF
    Ask {
        file: PathBuf,
        question: String,
        #[arg(short, long)]
        provider: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Interactive Q&A about a PDF
    Chat {
        file: PathBuf,
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// Show configured providers and key status
    Providers {
        #[arg(long)]
        json: bool,
    },
    /// Speak the JSON protocol over stdin/stdout (one JSON object per line)
    Stdio,
    /// Run the HTTP API
    #[cfg(feature = "http")]
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Require `Authorization: Bearer <token>` on every request
        #[arg(long, env = "PDFK_SERVER_TOKEN", hide_env_values = true)]
        token: Option<String>,
        /// Allow browser requests from this origin (repeatable)
        #[arg(long = "cors-origin")]
        cors_origins: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;

async fn run(cli: Cli) -> AnyResult<()> {
    if let Command::Stdio = cli.command {
        // Isolate before anything can print.
        let real_stdout = out::isolate_stdout()?;
        let engine = Arc::new(Engine::from_env()?);
        return Ok(stdio::run(engine, real_stdout).await?);
    }

    let machine_output = match &cli.command {
        Command::Summarize { json, .. } | Command::Ask { json, .. } | Command::Providers { json } => *json,
        _ => false,
    };
    let mut out: Box<dyn Write> = if machine_output {
        Box::new(out::isolate_stdout()?)
    } else {
        Box::new(std::io::stdout())
    };

    let engine = Arc::new(Engine::from_env()?);
    match cli.command {
        Command::Summarize { file, style, provider, json } => {
            let doc = engine.load_pdf_file(&file).await?;
            eprintln!("{}: {} pages, summarizing…", doc.name, doc.pages);
            let s = engine.summarize(&doc.id, style, provider).await?;
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&s)?)?;
            } else {
                writeln!(out, "{}\n", s.text)?;
                eprintln!("— {} · {}", s.provider, s.model);
            }
        }
        Command::Ask { file, question, provider, json } => {
            let doc = engine.load_pdf_file(&file).await?;
            let a = engine
                .ask(&doc.id, &question, AskOptions { provider, ..Default::default() })
                .await?;
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&a)?)?;
            } else {
                writeln!(out, "{}\n", a.text)?;
                eprintln!("— {} · {}", a.provider, a.model);
            }
        }
        Command::Chat { file, provider } => chat(&engine, file, provider).await?,
        Command::Providers { json } => {
            let list = engine.providers();
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&list)?)?;
            } else if list.is_empty() {
                writeln!(out, "No providers configured. Copy .env.example to .env and add keys.")?;
            } else {
                for p in list {
                    let keys = p
                        .keys
                        .map(|k| format!("{}/{} keys available", k.available, k.total))
                        .unwrap_or_else(|| "no key needed".into());
                    let model = p.model.unwrap_or_else(|| "auto".into());
                    writeln!(out, "{:<10} {:<28} {}  ({})", p.name, model, keys, p.base_url)?;
                }
            }
        }
        Command::Stdio => unreachable!("handled above"),
        #[cfg(feature = "http")]
        Command::Serve { host, port, token, cors_origins } => {
            http::serve(engine, &host, port, token, cors_origins).await?
        }
    }
    Ok(())
}

async fn chat(engine: &Engine, file: PathBuf, provider: Option<String>) -> AnyResult<()> {
    let doc = engine.load_pdf_file(&file).await?;
    eprintln!("Loaded {} ({} pages). Ask a question, or type /quit.", doc.name, doc.pages);
    let mut history: Vec<ChatMessage> = Vec::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    loop {
        out.write_all(b"\n> ").await?;
        out.flush().await?;
        let Some(line) = lines.next_line().await? else { break };
        let q = line.trim();
        if q.is_empty() {
            continue;
        }
        if matches!(q, "/quit" | "/exit" | "/q") {
            break;
        }
        let opts = AskOptions { provider: provider.clone(), history: history.clone(), top_k: None };
        match engine.ask(&doc.id, q, opts).await {
            Ok(a) => {
                println!("\n{}", a.text);
                history.push(ChatMessage::user(q));
                history.push(ChatMessage::assistant(a.text));
            }
            Err(e) => eprintln!("error: {e}"),
        }
    }
    Ok(())
}
