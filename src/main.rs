use llm_api::{Client, LIMIT, Provider, Request, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, BufRead, Read, Write};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    provider: Provider,
    request: Request,
    #[serde(default)]
    endpoint: Option<String>,
}

fn write_event(event: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, event)?;
    out.write_all(b"\n")?;
    out.flush()
}

fn run(command: Command) -> Result<()> {
    let mut client = if command.provider == Provider::Codex {
        Client::codex(
            std::env::var("CODEX_ACCESS_TOKEN")?,
            std::env::var("CODEX_ACCOUNT_ID")?,
        )?
    } else {
        let variable = match command.provider {
            Provider::Openai => "OPENAI_API_KEY",
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::Openrouter => "OPENROUTER_API_KEY",
            Provider::Codex => unreachable!(),
        };
        Client::new(command.provider, std::env::var(variable)?)
    };
    if let Some(endpoint) = command.endpoint {
        client = client.with_endpoint(endpoint)?;
    }
    let response = client.run(&command.request, |event| write_event(&event).is_ok())?;
    write_event(&json!({"type":"done","response":response}))?;
    Ok(())
}

fn main() -> Result<()> {
    let mut input = io::stdin().lock();
    loop {
        let mut line = String::new();
        let mut bounded = input.by_ref().take((LIMIT + 1) as u64);
        if bounded.read_line(&mut line)? == 0 {
            break;
        }
        if line.len() > LIMIT {
            return Err("command exceeds 16 MiB".into());
        }
        if line.trim().is_empty() {
            continue;
        }
        let result = serde_json::from_str::<Command>(&line)
            .map_err(Into::into)
            .and_then(run);
        if let Err(error) = result {
            write_event(&json!({"type":"error","message":error.to_string()}))?;
        }
    }
    Ok(())
}
