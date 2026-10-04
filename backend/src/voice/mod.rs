//! NPC voice: turns a spoken line into audio the game client can fetch.
//!
//! ```text
//! dialogue_started ─► VoiceService ─► VoiceProvider (ElevenLabs) ─► AudioCache
//!                          └─► audio_url "/audio/<id>"  ◄── GET /audio/{id}
//! ```
//!
//! Voice is optional and best-effort. [`VoiceService::voice_line`] never
//! fails: without a configured voice, on a provider error or on a timeout it
//! returns `None` and the dialogue is delivered as text only. Only the line
//! to be spoken ever leaves the process. See `docs/VOICE.md`.

mod cache;
mod elevenlabs;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use tracing::{debug, warn};

use crate::director::ProviderErrorKind;

pub use cache::{AudioCache, CacheLimits};
pub use elevenlabs::{ElevenLabsConfig, ElevenLabsVoiceProvider};

/// Largest clip accepted from a provider.
pub const MAX_AUDIO_BYTES: usize = 2 * 1024 * 1024;

/// How long one synthesis may take before the line goes out as text only.
pub const DEFAULT_VOICE_TIMEOUT: Duration = Duration::from_secs(6);

/// NPC ids and their voice ids, e.g. `hank_schrader=<voice_id>,walter_white=<voice_id>`.
pub const VOICES_ENV: &str = "ELEVENLABS_VOICES";

/// One line to speak. Nothing else about the NPC or the session is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceRequest<'a> {
    pub npc_id: &'a str,
    pub text: &'a str,
    pub voice_id: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceAudio {
    pub bytes: Vec<u8>,
    /// MIME type, e.g. `audio/mpeg`.
    pub content_type: String,
}

/// A voice provider failed. `detail` is safe to log: it never contains a
/// request URL, a header or a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{provider} {kind}: {detail}")]
pub struct VoiceError {
    pub provider: &'static str,
    pub kind: ProviderErrorKind,
    /// HTTP status, when there was one.
    pub status: Option<u16>,
    pub detail: String,
}

impl VoiceError {
    pub fn new(provider: &'static str, kind: ProviderErrorKind, detail: impl Into<String>) -> Self {
        Self {
            provider,
            kind,
            status: None,
            detail: detail.into(),
        }
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }
}

pub trait VoiceProvider: Send + Sync {
    /// Short stable name for logs, e.g. `elevenlabs`.
    fn name(&self) -> &'static str;

    fn synthesize<'a>(
        &'a self,
        request: VoiceRequest<'a>,
    ) -> BoxFuture<'a, Result<VoiceAudio, VoiceError>>;
}

/// The provider for a backend without voice: never synthesizes anything.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopVoiceProvider;

impl VoiceProvider for NoopVoiceProvider {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn synthesize<'a>(
        &'a self,
        _request: VoiceRequest<'a>,
    ) -> BoxFuture<'a, Result<VoiceAudio, VoiceError>> {
        Box::pin(async {
            Err(VoiceError::new(
                "noop",
                ProviderErrorKind::NotConfigured,
                "voice is not configured",
            ))
        })
    }
}

/// One canned provider response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptedVoice {
    Audio(VoiceAudio),
    Error(VoiceError),
    /// Never answers; for exercising timeouts.
    Hang,
}

/// What a [`ScriptedVoiceProvider`] was asked to speak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedVoiceCall {
    pub npc_id: String,
    pub text: String,
    pub voice_id: String,
}

/// A provider that plays back canned responses in order and records its
/// calls. For tests; it never touches the network.
#[derive(Debug, Default)]
pub struct ScriptedVoiceProvider {
    responses: Mutex<VecDeque<ScriptedVoice>>,
    calls: Mutex<Vec<RecordedVoiceCall>>,
}

impl ScriptedVoiceProvider {
    pub fn new(responses: impl IntoIterator<Item = ScriptedVoice>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            calls: Mutex::default(),
        }
    }

    pub fn calls(&self) -> Vec<RecordedVoiceCall> {
        self.calls
            .lock()
            .expect("voice calls lock poisoned")
            .clone()
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().expect("voice calls lock poisoned").len()
    }
}

impl VoiceProvider for ScriptedVoiceProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }

    fn synthesize<'a>(
        &'a self,
        request: VoiceRequest<'a>,
    ) -> BoxFuture<'a, Result<VoiceAudio, VoiceError>> {
        self.calls
            .lock()
            .expect("voice calls lock poisoned")
            .push(RecordedVoiceCall {
                npc_id: request.npc_id.to_owned(),
                text: request.text.to_owned(),
                voice_id: request.voice_id.to_owned(),
            });
        let response = self
            .responses
            .lock()
            .expect("voice responses lock poisoned")
            .pop_front();
        Box::pin(async move {
            match response {
                Some(ScriptedVoice::Audio(audio)) => Ok(audio),
                Some(ScriptedVoice::Error(error)) => Err(error),
                Some(ScriptedVoice::Hang) => std::future::pending().await,
                None => Err(VoiceError::new(
                    "scripted",
                    ProviderErrorKind::Unavailable,
                    "no scripted response left",
                )),
            }
        })
    }
}

/// Parse `npc_id=voice_id` pairs separated by commas. Voice ids end up in a
/// request path, so only ASCII letters and digits are accepted.
pub fn parse_voices(spec: &str) -> Result<BTreeMap<String, String>, String> {
    let mut voices = BTreeMap::new();
    for pair in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (npc_id, voice_id) = pair
            .split_once('=')
            .map(|(npc, voice)| (npc.trim(), voice.trim()))
            .ok_or_else(|| format!("{pair:?} is not npc_id=voice_id"))?;
        if npc_id.is_empty() {
            return Err(format!("{pair:?} has no npc id"));
        }
        if voice_id.is_empty() || !voice_id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(format!(
                "voice id for {npc_id:?} must be ASCII letters and digits"
            ));
        }
        voices.insert(npc_id.to_owned(), voice_id.to_owned());
    }
    Ok(voices)
}

/// Voices for NPCs, backed by one provider and the audio cache. Cheap to
/// clone; clones share the cache.
#[derive(Clone)]
pub struct VoiceService {
    provider: Arc<dyn VoiceProvider>,
    voices: Arc<BTreeMap<String, String>>,
    cache: AudioCache,
    timeout: Duration,
}

impl VoiceService {
    pub fn new(provider: Arc<dyn VoiceProvider>, voices: BTreeMap<String, String>) -> Self {
        Self {
            provider,
            voices: Arc::new(voices),
            cache: AudioCache::default(),
            timeout: DEFAULT_VOICE_TIMEOUT,
        }
    }

    /// ElevenLabs from the environment: `ELEVENLABS_API_KEY` and at least one
    /// voice in `ELEVENLABS_VOICES`. `NotConfigured` when either is missing.
    pub fn elevenlabs_from_env() -> Result<Self, VoiceError> {
        Self::elevenlabs_from_lookup(|key| std::env::var(key).ok())
    }

    pub(crate) fn elevenlabs_from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, VoiceError> {
        let not_configured = |detail: String| {
            VoiceError::new(
                ElevenLabsVoiceProvider::NAME,
                ProviderErrorKind::NotConfigured,
                detail,
            )
        };
        let config = ElevenLabsConfig::from_lookup(&lookup)?;
        let voices = parse_voices(&lookup(VOICES_ENV).unwrap_or_default())
            .map_err(|err| not_configured(format!("{VOICES_ENV}: {err}")))?;
        if voices.is_empty() {
            return Err(not_configured(format!("{VOICES_ENV} names no voice")));
        }
        let timeout = config.timeout;
        let provider = ElevenLabsVoiceProvider::new(config)?;
        Ok(Self::new(Arc::new(provider), voices).with_timeout(timeout))
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_cache(mut self, cache: AudioCache) -> Self {
        self.cache = cache;
        self
    }

    pub fn cache(&self) -> &AudioCache {
        &self.cache
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    /// NPC ids that have a voice.
    pub fn voiced_npcs(&self) -> impl Iterator<Item = &str> {
        self.voices.keys().map(String::as_str)
    }

    /// Synthesize `text` in the voice of `npc_id` and return the URL path the
    /// clip is served at. `None` (never an error) when the NPC has no voice
    /// or synthesis fails: the caller delivers the line as text.
    pub async fn voice_line(&self, npc_id: &str, text: &str) -> Option<String> {
        let voice_id = self.voices.get(npc_id)?;
        if text.trim().is_empty() {
            return None;
        }
        let request = VoiceRequest {
            npc_id,
            text,
            voice_id,
        };
        let provider = self.provider.name();
        let audio =
            match tokio::time::timeout(self.timeout, self.provider.synthesize(request)).await {
                Ok(Ok(audio)) => audio,
                Ok(Err(error)) => {
                    warn!(npc_id, %error, "voice synthesis failed; dialogue is text only");
                    return None;
                }
                Err(_) => {
                    warn!(
                        npc_id,
                        provider,
                        timeout_ms = self.timeout.as_millis() as u64,
                        "voice synthesis timed out; dialogue is text only"
                    );
                    return None;
                }
            };
        if audio.bytes.is_empty() || audio.bytes.len() > MAX_AUDIO_BYTES {
            warn!(
                npc_id,
                provider,
                bytes = audio.bytes.len(),
                "voice clip is empty or too large; dialogue is text only"
            );
            return None;
        }
        let bytes = audio.bytes.len();
        let id = self.cache.insert(audio)?;
        debug!(npc_id, provider, bytes, audio_id = %id, "voice clip cached");
        Some(format!("/audio/{id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip() -> VoiceAudio {
        VoiceAudio {
            bytes: vec![1, 2, 3],
            content_type: "audio/mpeg".into(),
        }
    }

    fn service(responses: Vec<ScriptedVoice>) -> (VoiceService, Arc<ScriptedVoiceProvider>) {
        let provider = Arc::new(ScriptedVoiceProvider::new(responses));
        let voices = parse_voices("hank_schrader=voiceHank1").unwrap();
        (VoiceService::new(provider.clone(), voices), provider)
    }

    fn audio_id(url: &str) -> uuid::Uuid {
        url.strip_prefix("/audio/").unwrap().parse().unwrap()
    }

    #[test]
    fn parses_voices() {
        let voices = parse_voices(" hank_schrader = abc123 ,walter_white=XyZ9,, ").unwrap();
        assert_eq!(voices.len(), 2);
        assert_eq!(voices["hank_schrader"], "abc123");
        assert_eq!(voices["walter_white"], "XyZ9");
        assert!(parse_voices("").unwrap().is_empty());
    }

    #[test]
    fn rejects_bad_voices() {
        for spec in ["hank", "=abc", "hank=", "hank=../v1/user", "hank=a b"] {
            assert!(parse_voices(spec).is_err(), "{spec}");
        }
    }

    #[tokio::test]
    async fn caches_the_clip_and_returns_its_url() {
        let (service, provider) = service(vec![ScriptedVoice::Audio(clip())]);
        let url = service.voice_line("hank_schrader", "Hello.").await.unwrap();
        assert_eq!(service.cache().get(audio_id(&url)), Some(clip()));
        assert_eq!(
            provider.calls(),
            [RecordedVoiceCall {
                npc_id: "hank_schrader".into(),
                text: "Hello.".into(),
                voice_id: "voiceHank1".into(),
            }]
        );
    }

    #[tokio::test]
    async fn an_npc_without_a_voice_makes_no_call() {
        let (service, provider) = service(vec![ScriptedVoice::Audio(clip())]);
        assert_eq!(service.voice_line("jesse_pinkman", "Yo.").await, None);
        assert_eq!(service.voice_line("hank_schrader", "  ").await, None);
        assert_eq!(provider.call_count(), 0);
    }

    #[tokio::test]
    async fn provider_failure_is_no_audio() {
        let unavailable = VoiceError::new("scripted", ProviderErrorKind::Unavailable, "HTTP 503");
        let (service, provider) = service(vec![ScriptedVoice::Error(unavailable)]);
        assert_eq!(service.voice_line("hank_schrader", "Hello.").await, None);
        assert_eq!(provider.call_count(), 1);
        assert!(service.cache().is_empty());
    }

    #[tokio::test]
    async fn timeout_is_no_audio() {
        let (service, _) = service(vec![ScriptedVoice::Hang]);
        let service = service.with_timeout(Duration::from_millis(30));
        assert_eq!(service.voice_line("hank_schrader", "Hello.").await, None);
        assert!(service.cache().is_empty());
    }

    #[tokio::test]
    async fn empty_and_oversized_clips_are_dropped() {
        let empty = VoiceAudio {
            bytes: Vec::new(),
            content_type: "audio/mpeg".into(),
        };
        let huge = VoiceAudio {
            bytes: vec![0; MAX_AUDIO_BYTES + 1],
            content_type: "audio/mpeg".into(),
        };
        let (service, _) = service(vec![
            ScriptedVoice::Audio(empty),
            ScriptedVoice::Audio(huge),
        ]);
        assert_eq!(service.voice_line("hank_schrader", "One.").await, None);
        assert_eq!(service.voice_line("hank_schrader", "Two.").await, None);
        assert!(service.cache().is_empty());
    }

    #[tokio::test]
    async fn noop_provider_is_no_audio() {
        let voices = parse_voices("hank_schrader=voiceHank1").unwrap();
        let service = VoiceService::new(Arc::new(NoopVoiceProvider), voices);
        assert_eq!(service.voice_line("hank_schrader", "Hello.").await, None);
    }

    #[test]
    fn from_env_needs_a_key_and_a_voice() {
        let lookup = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let kind = |pairs| {
            VoiceService::elevenlabs_from_lookup(lookup(pairs))
                .err()
                .map(|e| e.kind)
        };
        assert_eq!(kind(&[]), Some(ProviderErrorKind::NotConfigured));
        assert_eq!(
            kind(&[("ELEVENLABS_API_KEY", "k")]),
            Some(ProviderErrorKind::NotConfigured)
        );
        assert_eq!(
            kind(&[("ELEVENLABS_VOICES", "hank_schrader=abc")]),
            Some(ProviderErrorKind::NotConfigured)
        );
        assert_eq!(
            kind(&[
                ("ELEVENLABS_API_KEY", "k"),
                ("ELEVENLABS_VOICES", "hank_schrader=bad/id")
            ]),
            Some(ProviderErrorKind::NotConfigured)
        );
        let service = VoiceService::elevenlabs_from_lookup(lookup(&[
            ("ELEVENLABS_API_KEY", "k"),
            ("ELEVENLABS_VOICES", "hank_schrader=abc,walter_white=def"),
            ("ELEVENLABS_TIMEOUT_MS", "1500"),
        ]))
        .unwrap();
        assert_eq!(service.provider_name(), "elevenlabs");
        assert_eq!(
            service.voiced_npcs().collect::<Vec<_>>(),
            ["hank_schrader", "walter_white"]
        );
        assert_eq!(service.timeout, Duration::from_millis(1500));
    }
}
