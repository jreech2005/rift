//! Live ElevenLabs check for NPC voice.
//!
//! Not part of the default suite: it needs a real `ELEVENLABS_API_KEY` and at
//! least one voice in `ELEVENLABS_VOICES` (repo-root `.env`), and it spends a
//! few characters of quota. Run it with
//!
//! ```sh
//! make voice-live
//! ```
//!
//! Without credentials the test FAILS with `BLOCKED` — it never reports a
//! synthesis it did not make.

use std::path::Path;

use rift_backend::voice::VoiceService;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires live ElevenLabs credentials"]
async fn live_elevenlabs_synthesis() {
    let _ = dotenvy::from_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env"));
    let voice = match VoiceService::elevenlabs_from_env() {
        Ok(voice) => voice,
        Err(err) => panic!("ElevenLabs live test BLOCKED: {}", err.detail),
    };
    let npc_id = voice.voiced_npcs().next().unwrap().to_owned();

    let url = voice
        .voice_line(&npc_id, "Rift voice check.")
        .await
        .expect("ElevenLabs returned audio");
    let id: Uuid = url.strip_prefix("/audio/").unwrap().parse().unwrap();
    let clip = voice.cache().get(id).expect("clip is cached");
    assert!(!clip.bytes.is_empty());
    println!(
        "synthesized {} bytes of {} for {npc_id}",
        clip.bytes.len(),
        clip.content_type
    );
}
