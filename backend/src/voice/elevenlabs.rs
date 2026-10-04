//! ElevenLabs text-to-speech.
//!
//! One request per line: `POST /v1/text-to-speech/{voice_id}` with the text
//! to speak and nothing else. The API key travels only in the `xi-api-key`
//! header and is never logged; errors carry a category and an HTTP status,
//! not the provider's response text.

use std::time::Duration;

use futures_util::future::BoxFuture;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use serde_json::json;

use super::{
    DEFAULT_VOICE_TIMEOUT, MAX_AUDIO_BYTES, VoiceAudio, VoiceError, VoiceProvider, VoiceRequest,
};
use crate::director::{ProviderErrorKind, Secret};

const PROVIDER: &str = "elevenlabs";
const DEFAULT_BASE_URL: &str = "https://api.elevenlabs.io";
/// Low-latency model: a line should be ready within about a second.
const DEFAULT_MODEL: &str = "eleven_flash_v2_5";
const DEFAULT_OUTPUT_FORMAT: &str = "mp3_44100_128";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElevenLabsConfig {
    pub api_key: Secret,
    pub model: String,
    /// ElevenLabs `output_format`, e.g. `mp3_44100_128`. A `pcm_<rate>`
    /// format is served as a WAV file (what the Unreal client plays).
    pub output_format: String,
    /// Overridable so tests can point at a local server.
    pub base_url: String,
    pub timeout: Duration,
}

impl ElevenLabsConfig {
    pub fn new(api_key: Secret) -> Self {
        Self {
            api_key,
            model: DEFAULT_MODEL.to_owned(),
            output_format: DEFAULT_OUTPUT_FORMAT.to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout: DEFAULT_VOICE_TIMEOUT,
        }
    }

    pub fn from_env() -> Result<Self, VoiceError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub(crate) fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, VoiceError> {
        let non_empty = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let not_configured =
            |detail: &str| VoiceError::new(PROVIDER, ProviderErrorKind::NotConfigured, detail);

        let api_key = non_empty("ELEVENLABS_API_KEY")
            .ok_or_else(|| not_configured("ELEVENLABS_API_KEY is not set"))?;
        let mut config = Self::new(Secret::new(api_key));
        if let Some(model) = non_empty("ELEVENLABS_MODEL") {
            config.model = model;
        }
        if let Some(format) = non_empty("ELEVENLABS_OUTPUT_FORMAT") {
            // Goes into the query string.
            if !format
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(not_configured(
                    "ELEVENLABS_OUTPUT_FORMAT is not a format name",
                ));
            }
            config.output_format = format;
        }
        if let Some(ms) = non_empty("ELEVENLABS_TIMEOUT_MS") {
            let ms: u64 =
                ms.parse().ok().filter(|ms| *ms > 0).ok_or_else(|| {
                    not_configured("ELEVENLABS_TIMEOUT_MS is not a positive number")
                })?;
            config.timeout = Duration::from_millis(ms);
        }
        Ok(config)
    }
}

#[derive(Debug, Clone)]
pub struct ElevenLabsVoiceProvider {
    client: reqwest::Client,
    config: ElevenLabsConfig,
}

impl ElevenLabsVoiceProvider {
    pub const NAME: &'static str = PROVIDER;

    pub fn new(config: ElevenLabsConfig) -> Result<Self, VoiceError> {
        // The format is written into the request URL.
        let format_ok = |c: char| c.is_ascii_alphanumeric() || c == '_';
        if config.output_format.is_empty() || !config.output_format.chars().all(format_ok) {
            return Err(VoiceError::new(
                PROVIDER,
                ProviderErrorKind::NotConfigured,
                "output format is not a format name",
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|_| {
                VoiceError::new(
                    PROVIDER,
                    ProviderErrorKind::Network,
                    "HTTP client could not be initialised",
                )
            })?;
        Ok(Self { client, config })
    }

    pub fn from_env() -> Result<Self, VoiceError> {
        Self::new(ElevenLabsConfig::from_env()?)
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    async fn speak(&self, request: VoiceRequest<'_>) -> Result<VoiceAudio, VoiceError> {
        let error = |kind, detail: &str| VoiceError::new(PROVIDER, kind, detail);

        if !request.voice_id.chars().all(|c| c.is_ascii_alphanumeric())
            || request.voice_id.is_empty()
        {
            return Err(error(
                ProviderErrorKind::NotConfigured,
                "voice id must be ASCII letters and digits",
            ));
        }
        let mut key = HeaderValue::from_str(self.config.api_key.expose()).map_err(|_| {
            error(
                ProviderErrorKind::NotConfigured,
                "ELEVENLABS_API_KEY contains characters that cannot be sent in a header",
            )
        })?;
        key.set_sensitive(true);

        let url = format!(
            "{}/v1/text-to-speech/{}?output_format={}",
            self.config.base_url.trim_end_matches('/'),
            request.voice_id,
            self.config.output_format
        );
        // The spoken line and the model: nothing about the session, the NPC's
        // memories or the player.
        let body = json!({ "text": request.text, "model_id": self.config.model });
        let response = self
            .client
            .post(url)
            .header("xi-api-key", key)
            .json(&body)
            .send()
            .await
            .map_err(transport_error)?;

        let status = response.status();
        if !status.is_success() {
            let kind = match status.as_u16() {
                401 | 403 => ProviderErrorKind::Auth,
                429 => ProviderErrorKind::RateLimited,
                500.. => ProviderErrorKind::Unavailable,
                _ => ProviderErrorKind::Http,
            };
            return Err(
                error(kind, &format!("HTTP {}", status.as_u16())).with_status(status.as_u16())
            );
        }
        let malformed =
            |detail: &str| error(ProviderErrorKind::Malformed, detail).with_status(status.as_u16());

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase())
            .unwrap_or_default();
        if !(content_type.starts_with("audio/") || content_type == "application/octet-stream") {
            return Err(malformed("response is not audio"));
        }
        let declared = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok());
        if declared.is_some_and(|len| len > MAX_AUDIO_BYTES) {
            return Err(malformed("audio is larger than the clip limit"));
        }
        let bytes = response.bytes().await.map_err(transport_error)?;
        if bytes.is_empty() {
            return Err(malformed("audio is empty"));
        }
        if bytes.len() > MAX_AUDIO_BYTES {
            return Err(malformed("audio is larger than the clip limit"));
        }
        // `pcm_<rate>` is headerless 16-bit mono: add the WAV header so the
        // clip describes itself to whoever fetches it.
        if let Some(sample_rate) = pcm_sample_rate(&self.config.output_format) {
            return Ok(VoiceAudio {
                bytes: wav_from_pcm16_mono(&bytes, sample_rate),
                content_type: "audio/wav".to_owned(),
            });
        }
        Ok(VoiceAudio {
            bytes: bytes.to_vec(),
            content_type,
        })
    }
}

/// The sample rate of an ElevenLabs raw PCM format name (`pcm_24000`).
fn pcm_sample_rate(output_format: &str) -> Option<u32> {
    output_format
        .strip_prefix("pcm_")?
        .parse()
        .ok()
        .filter(|rate| *rate > 0)
}

/// Wrap signed 16-bit little-endian mono samples in a 44-byte WAV header.
fn wav_from_pcm16_mono(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    // Clips are capped at MAX_AUDIO_BYTES, far below the 4 GiB WAV limit.
    let data_len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // bytes per second
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

impl VoiceProvider for ElevenLabsVoiceProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn synthesize<'a>(
        &'a self,
        request: VoiceRequest<'a>,
    ) -> BoxFuture<'a, Result<VoiceAudio, VoiceError>> {
        Box::pin(self.speak(request))
    }
}

/// Classify a transport failure. Only a category is reported: reqwest's own
/// message can include the request URL.
fn transport_error(err: reqwest::Error) -> VoiceError {
    if err.is_timeout() {
        VoiceError::new(
            PROVIDER,
            ProviderErrorKind::Timeout,
            "no response within the timeout",
        )
    } else if err.is_connect() {
        VoiceError::new(PROVIDER, ProviderErrorKind::Network, "connection failed")
    } else {
        VoiceError::new(PROVIDER, ProviderErrorKind::Network, "request failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "unit-key-do-not-leak";

    #[test]
    fn only_raw_pcm_formats_are_wrapped_as_wav() {
        assert_eq!(pcm_sample_rate("pcm_24000"), Some(24_000));
        assert_eq!(pcm_sample_rate("pcm_16000"), Some(16_000));
        assert_eq!(pcm_sample_rate("mp3_44100_128"), None);
        assert_eq!(pcm_sample_rate("pcm_"), None);
        assert_eq!(pcm_sample_rate("pcm_0"), None);

        let wav = wav_from_pcm16_mono(&[1, 2, 3, 4], 24_000);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(wav[4..8], 40u32.to_le_bytes());
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(wav[22..24], 1u16.to_le_bytes());
        assert_eq!(wav[24..28], 24_000u32.to_le_bytes());
        assert_eq!(wav[28..32], 48_000u32.to_le_bytes());
        assert_eq!(wav[40..44], 4u32.to_le_bytes());
        assert_eq!(&wav[44..], &[1, 2, 3, 4]);
    }

    #[test]
    fn config_defaults_and_overrides() {
        let config = ElevenLabsConfig::from_lookup(|key| match key {
            "ELEVENLABS_API_KEY" => Some(format!("  {KEY} ")),
            _ => None,
        })
        .unwrap();
        assert_eq!(config.api_key.expose(), KEY);
        assert_eq!(config.model, DEFAULT_MODEL);
        assert_eq!(config.output_format, DEFAULT_OUTPUT_FORMAT);
        assert_eq!(config.timeout, DEFAULT_VOICE_TIMEOUT);

        let config = ElevenLabsConfig::from_lookup(|key| match key {
            "ELEVENLABS_API_KEY" => Some(KEY.into()),
            "ELEVENLABS_MODEL" => Some("eleven_turbo_v2_5".into()),
            "ELEVENLABS_OUTPUT_FORMAT" => Some("pcm_24000".into()),
            "ELEVENLABS_TIMEOUT_MS" => Some("2500".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(config.model, "eleven_turbo_v2_5");
        assert_eq!(config.output_format, "pcm_24000");
        assert_eq!(config.timeout, Duration::from_millis(2500));
    }

    #[test]
    fn config_rejects_missing_key_and_bad_values() {
        let kind = |pairs: &[(&str, &str)]| {
            ElevenLabsConfig::from_lookup(|key| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            })
            .unwrap_err()
            .kind
        };
        assert_eq!(kind(&[]), ProviderErrorKind::NotConfigured);
        assert_eq!(
            kind(&[("ELEVENLABS_API_KEY", "  ")]),
            ProviderErrorKind::NotConfigured
        );
        assert_eq!(
            kind(&[
                ("ELEVENLABS_API_KEY", KEY),
                ("ELEVENLABS_OUTPUT_FORMAT", "mp3&x=1")
            ]),
            ProviderErrorKind::NotConfigured
        );
        assert_eq!(
            kind(&[("ELEVENLABS_API_KEY", KEY), ("ELEVENLABS_TIMEOUT_MS", "0")]),
            ProviderErrorKind::NotConfigured
        );
    }

    #[test]
    fn debug_output_hides_the_key() {
        let provider =
            ElevenLabsVoiceProvider::new(ElevenLabsConfig::new(Secret::new(KEY))).unwrap();
        assert!(!format!("{provider:?}").contains(KEY));
    }
}
