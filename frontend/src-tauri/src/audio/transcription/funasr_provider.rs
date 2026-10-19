use super::provider::{TranscriptResult, TranscriptionError, TranscriptionProvider};
use async_trait::async_trait;
use std::sync::Arc;

pub struct FunAsrProvider {
    engine: Arc<crate::funasr_engine::FunAsrEngine>,
}

impl FunAsrProvider {
    pub fn new(engine: Arc<crate::funasr_engine::FunAsrEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl TranscriptionProvider for FunAsrProvider {
    async fn transcribe(
        &self,
        audio: Vec<f32>,
        language: Option<String>,
    ) -> std::result::Result<TranscriptResult, TranscriptionError> {
        if audio.len() < 1600 {
            return Err(TranscriptionError::AudioTooShort {
                samples: audio.len(),
                minimum: 1600,
            });
        }
        let requested_language = language.unwrap_or_else(|| "auto".to_string());
        let language = crate::funasr_engine::normalize_language(Some(&requested_language))
            .map_err(|_| TranscriptionError::UnsupportedLanguage(requested_language))?;
        let text = self
            .engine
            .transcribe(audio, language)
            .await
            .map_err(|error| TranscriptionError::EngineFailed(error.to_string()))?;
        Ok(TranscriptResult {
            text: text.trim().to_string(),
            confidence: None,
            is_partial: false,
        })
    }

    async fn is_model_loaded(&self) -> bool {
        self.engine.is_model_loaded().await
    }

    async fn get_current_model(&self) -> Option<String> {
        self.engine.get_current_model().await
    }

    async fn unload_model(&self) -> bool {
        self.engine.unload_model().await
    }

    fn provider_name(&self) -> &'static str {
        "FUN-ASR"
    }
}
