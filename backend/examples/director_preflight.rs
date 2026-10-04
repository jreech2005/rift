//! Pre-demo health check for the Director's LLM chain.
//!
//! ```sh
//! make director-preflight
//! cargo run --manifest-path backend/Cargo.toml --example director_preflight
//! ```
//!
//! Reads the same environment as the backend (repo-root `.env`, then the
//! process environment) and sends ONE tiny request to each configured model,
//! in failover order. It creates no session, touches no game state and is
//! never run by the backend or the tests. Keys are never printed.
//!
//! Exit code: `0` the primary model is healthy, `1` the primary is down but a
//! later model is healthy, `2` no LLM is healthy or configured (the game
//! still runs, on the deterministic rules).

use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use rift_backend::director::DirectorSettings;

#[tokio::main]
async fn main() -> ExitCode {
    let root_env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    let _ = dotenvy::from_path(&root_env);
    let _ = dotenvy::dotenv();

    let settings = DirectorSettings::from_env();
    println!("Director preflight");
    println!(
        "  transient retries (primary): {}, total budget: {} ms",
        settings.transient_retries,
        settings.budget.as_millis()
    );
    if settings.llms.is_empty() {
        println!("  no LLM configured (GEMINI_API_KEY / ANTHROPIC_API_KEY)");
        println!("RESULT: deterministic rules only");
        return ExitCode::from(2);
    }

    let mut healthy = Vec::new();
    for (index, (llm, timeout)) in settings.llms.iter().enumerate() {
        let started = Instant::now();
        let result = match tokio::time::timeout(*timeout, llm.ping()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("{}: {}", error.kind, error.detail)),
            Err(_) => Err(format!(
                "timeout: no response within {} ms",
                timeout.as_millis()
            )),
        };
        let elapsed = started.elapsed().as_millis();
        match result {
            Ok(()) => {
                println!("  {}. {}: HEALTHY ({elapsed} ms)", index + 1, llm.label());
                healthy.push(index);
            }
            Err(reason) => println!(
                "  {}. {}: UNAVAILABLE ({reason}, {elapsed} ms)",
                index + 1,
                llm.label()
            ),
        }
    }

    match healthy.first() {
        Some(0) => {
            println!("RESULT: primary healthy");
            ExitCode::SUCCESS
        }
        Some(&index) => {
            println!(
                "RESULT: primary down; {} will answer after failover",
                settings.llms[index].0.label()
            );
            ExitCode::from(1)
        }
        None => {
            println!("RESULT: no LLM healthy; the deterministic rules will answer");
            ExitCode::from(2)
        }
    }
}
