pub mod commands;
pub mod engine;
pub mod model;
mod sidecar;

pub use engine::FunAsrEngine;
pub(crate) use sidecar::MAX_AUDIO_SAMPLES;

pub fn normalize_language(language: Option<&str>) -> Result<&'static str, String> {
    let language = language.unwrap_or("auto");
    if language == "auto" || language.is_empty() {
        Ok("auto")
    } else if language.starts_with("en") {
        Ok("en")
    } else if language.starts_with("zh") {
        Ok("zh")
    } else if language.starts_with("ja") {
        Ok("ja")
    } else {
        Err(format!(
            "Language '{language}' is not supported by FUN-ASR Nano. Choose Auto, English, Chinese, or Japanese."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_mapping_accepts_supported_prefixes() {
        assert_eq!(normalize_language(None).unwrap(), "auto");
        assert_eq!(normalize_language(Some("en-US")).unwrap(), "en");
        assert_eq!(normalize_language(Some("zh-CN")).unwrap(), "zh");
        assert_eq!(normalize_language(Some("ja")).unwrap(), "ja");
    }

    #[test]
    fn language_mapping_rejects_unsupported_languages() {
        assert!(normalize_language(Some("fr")).is_err());
        assert!(normalize_language(Some("auto-translate")).is_err());
    }
}
