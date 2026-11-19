//! Optional, installation-owned samples for the configured voice model. The
//! browser never contacts the provider (or supplies a URL or filesystem path).
use super::{VOICE_MODEL, VOICES};
use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::AppState,
};
use anyhow::{Result, ensure};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, header},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path as FsPath, sync::Arc};

const MAX_AUDIO: usize = 44 + 24_000 * 2 * 12;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Index {
    provider: String,
    model: String,
    samples: BTreeMap<String, Sample>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sample {
    sha256: String,
    /// Installation operator's origin/reference, kept private to the installation.
    source: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VoiceTraits {
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presentation: Option<String>,
}

/// Expose only provider-documented traits of voices selectable by this bridge.
pub(super) fn traits() -> Option<&'static BTreeMap<String, VoiceTraits>> {
    #[derive(Deserialize)]
    struct Catalog {
        provider: String,
        model: String,
        voices: BTreeMap<String, VoiceTraits>,
    }
    static LIVE: std::sync::LazyLock<Catalog> = std::sync::LazyLock::new(|| {
        let mut catalog: Catalog = serde_json::from_str(include_str!(
            "../bridge/openai-live-voice-descriptions.json"
        ))
        .expect("valid built-in OpenAI catalog");
        catalog.voices.retain(|id, _| VOICES.contains(&id.as_str()));
        catalog
    });
    (LIVE.provider == "OpenAI" && LIVE.model == VOICE_MODEL).then_some(&LIVE.voices)
}

pub(super) fn descriptions(traits: &BTreeMap<String, VoiceTraits>) -> BTreeMap<&str, String> {
    traits
        .iter()
        .map(|(voice, fields)| {
            let description = [
                fields.language.as_deref(),
                fields.region.as_deref(),
                fields.presentation.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            (voice.as_str(), description)
        })
        .collect()
}

fn bounded_file(path: &FsPath, limit: usize) -> Result<Vec<u8>> {
    let metadata = crate::native_browser_update::ordinary(path, false)?;
    ensure!(metadata.len() <= limit as u64, "Preview exceeds size limit");
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "Preview exceeds size limit");
    Ok(bytes)
}

pub(super) fn index(home: &FsPath) -> Option<Index> {
    let directory = home.join("voice-previews");
    amplifier_native_platform::validate_private_directory(&directory).ok()?;
    let index: Index =
        serde_json::from_slice(&bounded_file(&directory.join("index.json"), 16_384).ok()?).ok()?;
    (index.provider == "OpenAI" && index.model == VOICE_MODEL).then_some(index)
}

impl Index {
    fn sample(&self, voice: &str) -> Option<&Sample> {
        if !VOICES.contains(&voice) {
            return None;
        }
        self.samples.get(voice).filter(|sample| {
            !sample.source.trim().is_empty()
                && sample.source.len() <= 512
                && sample.sha256.len() == 64
                && sample
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
    }
    pub(super) fn voices(&self) -> Vec<&str> {
        VOICES
            .iter()
            .copied()
            .filter(|voice| self.sample(voice).is_some())
            .collect()
    }
}

fn pcm_wav(bytes: &[u8]) -> bool {
    // Accept one canonical mono 24 kHz signed 16-bit PCM stream. No containers
    // with extra chunks, external references, or trailing metadata are served.
    bytes.len() > 44
        && bytes.len() <= MAX_AUDIO
        && bytes.len().is_multiple_of(2)
        && &bytes[..4] == b"RIFF"
        && bytes[4..8] == ((bytes.len() - 8) as u32).to_le_bytes()
        && &bytes[8..16] == b"WAVEfmt "
        && bytes[16..20] == 16u32.to_le_bytes()
        && bytes[20..24] == [1, 0, 1, 0]
        && bytes[24..28] == 24000u32.to_le_bytes()
        && bytes[28..32] == 48000u32.to_le_bytes()
        && bytes[32..36] == [2, 0, 16, 0]
        && &bytes[36..40] == b"data"
        && bytes[40..44] == ((bytes.len() - 44) as u32).to_le_bytes()
}

fn audio(home: &FsPath, voice: &str) -> Option<Vec<u8>> {
    let index = index(home)?;
    let sample = index.sample(voice)?;
    let bytes = bounded_file(
        &home.join("voice-previews").join(format!("{voice}.wav")),
        MAX_AUDIO,
    )
    .ok()?;
    (pcm_wav(&bytes) && format!("{:x}", Sha256::digest(&bytes)) == sample.sha256).then_some(bytes)
}

pub async fn read(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, voice)): Path<(String, String)>,
) -> ApiResult<impl axum::response::IntoResponse> {
    auth::check(&s, &h, "observe", false)?;
    let config = super::config(&s).map_err(|_| ApiError::missing())?;
    if !config.profiles.contains_key(&id) || s.agent(&id)?.is_none() {
        return Err(ApiError::missing());
    }
    let bytes = audio(&s.home, &voice).ok_or_else(ApiError::missing)?;
    Ok(([(header::CONTENT_TYPE, "audio/wav")], bytes))
}
