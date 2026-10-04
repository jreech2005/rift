//! Live Director check: ONE decision for the divergence scenario.
//!
//! ```sh
//! cargo run --manifest-path backend/Cargo.toml --example director_live
//! cargo run --manifest-path backend/Cargo.toml --example director_live -- --offline
//! cargo run --manifest-path backend/Cargo.toml --example director_live -- path/to/world_bible.json
//! ```
//!
//! Scenario: the player was asked to hide Walter's burner phone and told Hank
//! about it instead. The WorldBible defaults to the Phase 1 Breaking Bad one.
//!
//! Makes at most two Gemini requests (the decision, plus the single repair
//! attempt if the first output is rejected) and never retries a provider
//! failure. `--offline` uses the deterministic rules instead of Gemini.
//!
//! Exit codes: 0 validated decision, 1 output rejected, 2 blocked (no key,
//! provider unavailable, unreadable input).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use rift_backend::director::{
    DirectorContext, DirectorEngine, DirectorError, DirectorProvider, FallbackDirector,
    GeminiDirector, WorldBibleView, WorldSummary,
};
use serde_json::Value;

const SCENARIO: &str = include_str!("../tests/fixtures/director/scenario_hank_disclosure.json");
const DEFAULT_WORLD_BIBLE: &str = "tests/fixtures/director/world_bible_breaking_bad.json";

fn build_context(world_bible: &Path) -> Result<DirectorContext, String> {
    let text = std::fs::read_to_string(world_bible)
        .map_err(|e| format!("cannot read {}: {e}", world_bible.display()))?;
    let bible = WorldBibleView::from_json_str(&text).map_err(|e| e.to_string())?;
    let mut scenario: Value = serde_json::from_str(SCENARIO).map_err(|e| e.to_string())?;
    scenario["universe_id"] = Value::String(bible.universe.universe_id.clone());
    scenario["world"] =
        serde_json::to_value(WorldSummary::from_world_bible(&bible)).map_err(|e| e.to_string())?;
    let ctx: DirectorContext = serde_json::from_value(scenario).map_err(|e| e.to_string())?;
    ctx.validate().map_err(|issues| {
        let lines: Vec<String> = issues.iter().map(ToString::to_string).collect();
        format!(
            "scenario does not fit this WorldBible: {}",
            lines.join("; ")
        )
    })?;
    Ok(ctx)
}

#[tokio::main]
async fn main() -> ExitCode {
    // Same .env handling as the server binary. Values are never printed.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let _ = dotenvy::from_path(root.join("../.env"));
    let _ = dotenvy::dotenv();

    let mut offline = false;
    let mut world_bible: PathBuf = root.join(DEFAULT_WORLD_BIBLE);
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--offline" => offline = true,
            path => world_bible = PathBuf::from(path),
        }
    }

    let ctx = match build_context(&world_bible) {
        Ok(ctx) => ctx,
        Err(message) => {
            eprintln!("BLOCKED: {message}");
            return ExitCode::from(2);
        }
    };
    let context_bytes = serde_json::to_vec(&ctx).map(|b| b.len()).unwrap_or(0);
    println!("Universe: {} ({})", ctx.world.title, ctx.universe_id);
    println!("Trigger: {}", ctx.trigger.kind());
    println!("DirectorContext: {context_bytes} bytes");
    let schema = rift_backend::director::schema::gemini_response_schema(&ctx);
    println!(
        "Response schema: {} bytes, {} action types offered",
        schema.to_string().len(),
        schema["properties"]["actions"]["items"]["anyOf"]
            .as_array()
            .map_or(0, Vec::len)
    );

    let provider: Arc<dyn DirectorProvider> = if offline {
        Arc::new(FallbackDirector)
    } else {
        match GeminiDirector::from_env() {
            Ok(gemini) => {
                println!("Asking Gemini ({})...", gemini.model());
                Arc::new(gemini)
            }
            Err(error) => {
                eprintln!("BLOCKED: {error}");
                return ExitCode::from(2);
            }
        }
    };

    match DirectorEngine::new(provider).decide(&ctx).await {
        Ok(decision) => {
            let json = serde_json::to_string_pretty(&decision).unwrap_or_default();
            println!("{json}");
            println!(
                "DirectorDecision validation: PASS (provider: {}, model: {}, attempts: {}, actions: {}, {} ms)",
                decision.metadata.provider,
                decision.metadata.model,
                decision.metadata.attempts,
                decision.actions.len(),
                decision.metadata.latency_ms
            );
            ExitCode::SUCCESS
        }
        Err(DirectorError::InvalidDecision { attempts, issues }) => {
            eprintln!("FAILED: output rejected after {attempts} attempt(s):");
            for issue in issues {
                eprintln!("  - {issue}");
            }
            ExitCode::from(1)
        }
        Err(error) => {
            eprintln!("BLOCKED: {error}");
            ExitCode::from(2)
        }
    }
}
