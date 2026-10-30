use crate::database::models::{Setting, TranscriptSetting};
use crate::summary::CustomOpenAIConfig;
use sqlx::SqlitePool;

#[derive(serde::Deserialize, Debug)]
pub struct SaveModelConfigRequest {
    pub provider: String,
    pub model: String,
    #[serde(rename = "whisperModel")]
    pub whisper_model: String,
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    #[serde(rename = "ollamaEndpoint")]
    pub ollama_endpoint: Option<String>,
}

#[derive(serde::Deserialize, Debug)]
pub struct SaveTranscriptConfigRequest {
    pub provider: String,
    pub model: String,
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
}

pub struct SettingsRepository;

// Transcript providers: localWhisper, deepgram, elevenLabs, groq, openai
// Summary providers: openai, claude, ollama, groq, added openrouter
// NOTE: Handle data exclusion in the higher layer as this is database abstraction layer(using SELECT *)

impl SettingsRepository {
    pub async fn get_model_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<Setting>, sqlx::Error> {
        let setting = sqlx::query_as::<_, Setting>("SELECT * FROM settings LIMIT 1")
            .fetch_optional(pool)
            .await?;
        Ok(setting)
    }

    pub async fn save_model_config(
        pool: &SqlitePool,
        provider: &str,
        model: &str,
        whisper_model: &str,
        ollama_endpoint: Option<&str>,
    ) -> std::result::Result<(), sqlx::Error> {
        // Using id '1' for backward compatibility
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, ollamaEndpoint)
            VALUES ('1', $1, $2, $3, $4)
            ON CONFLICT(id) DO UPDATE SET
                provider = excluded.provider,
                model = excluded.model,
                whisperModel = excluded.whisperModel,
                ollamaEndpoint = excluded.ollamaEndpoint
            "#,
        )
        .bind(provider)
        .bind(model)
        .bind(whisper_model)
        .bind(ollama_endpoint)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn save_api_key(
        pool: &SqlitePool,
        provider: &str,
        api_key: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        // Custom OpenAI uses JSON config (customOpenAIConfig) instead of a separate API key column
        if provider == "custom-openai" {
            return Err(sqlx::Error::Protocol(
                "custom-openai provider should use save_custom_openai_config() instead of save_api_key()".into(),
            ));
        }

        let api_key_column = match provider {
            "openai" => "openaiApiKey",
            "claude" => "anthropicApiKey",
            "ollama" => "ollamaApiKey",
            "groq" => "groqApiKey",
            "openrouter" => "openRouterApiKey",
            "builtin-ai" => return Ok(()), // No API key needed
            _ => {
                return Err(sqlx::Error::Protocol(
                    format!("Invalid provider: {}", provider).into(),
                ))
            }
        };

        let query = format!(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, "{}")
            VALUES ('1', 'openai', 'gpt-4o-2024-11-20', 'large-v3', $1)
            ON CONFLICT(id) DO UPDATE SET
                "{}" = $1
            "#,
            api_key_column, api_key_column
        );
        sqlx::query(&query).bind(api_key).execute(pool).await?;

        Ok(())
    }

    pub async fn get_api_key(
        pool: &SqlitePool,
        provider: &str,
    ) -> std::result::Result<Option<String>, sqlx::Error> {
        // Custom OpenAI uses JSON config - extract API key from there
        if provider == "custom-openai" {
            let config = Self::get_custom_openai_config(pool).await?;
            return Ok(config.and_then(|c| c.api_key));
        }

        let api_key_column = match provider {
            "openai" => "openaiApiKey",
            "ollama" => "ollamaApiKey",
            "groq" => "groqApiKey",
            "claude" => "anthropicApiKey",
            "openrouter" => "openRouterApiKey",
            "builtin-ai" => return Ok(None), // No API key needed
            _ => {
                return Err(sqlx::Error::Protocol(
                    format!("Invalid provider: {}", provider).into(),
                ))
            }
        };

        let query = format!(
            "SELECT {} FROM settings WHERE id = '1' LIMIT 1",
            api_key_column
        );
        let api_key: Option<Option<String>> = sqlx::query_scalar(&query).fetch_optional(pool).await?;
        Ok(api_key.flatten())
    }

    pub async fn get_transcript_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<TranscriptSetting>, sqlx::Error> {
        let setting =
            sqlx::query_as::<_, TranscriptSetting>("SELECT * FROM transcript_settings LIMIT 1")
                .fetch_optional(pool)
                .await?;
        Ok(setting)

    }

    pub async fn save_transcript_config(
        pool: &SqlitePool,
        provider: &str,
        model: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO transcript_settings (id, provider, model)
            VALUES ('1', $1, $2)
            ON CONFLICT(id) DO UPDATE SET
                provider = excluded.provider,
                model = excluded.model
            "#,
        )
        .bind(provider)
        .bind(model)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn save_transcript_api_key(
        pool: &SqlitePool,
        provider: &str,
        api_key: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        let api_key_column = match provider {
            "localWhisper" => "whisperApiKey",
            // Local models do not use an API key. FUN-ASR used to fall through
            // to the error arm, which made api_get_transcript_config fail for
            // every saved FUN-ASR configuration.
            "parakeet" | "funasr" => return Ok(()),
            "deepgram" => "deepgramApiKey",
            "elevenLabs" => "elevenLabsApiKey",
            "groq" => "groqApiKey",
            "openai" => "openaiApiKey",
            _ => {
                return Err(sqlx::Error::Protocol(
                    format!("Invalid provider: {}", provider).into(),
                ))
            }
        };

        let query = format!(
            r#"
            INSERT INTO transcript_settings (id, provider, model, "{}")
            VALUES ('1', 'parakeet', '{}', $1)
            ON CONFLICT(id) DO UPDATE SET
                "{}" = $1
            "#,
            api_key_column, crate::config::DEFAULT_PARAKEET_MODEL, api_key_column
        );
        sqlx::query(&query).bind(api_key).execute(pool).await?;

        Ok(())
    }

    pub async fn get_transcript_api_key(
        pool: &SqlitePool,
        provider: &str,
    ) -> std::result::Result<Option<String>, sqlx::Error> {
        let api_key_column = match provider {
            "localWhisper" => "whisperApiKey",
            // Local models do not use an API key. FUN-ASR used to fall through
            // to the error arm, which made api_get_transcript_config fail for
            // every saved FUN-ASR configuration.
            "parakeet" | "funasr" => return Ok(None),
            "deepgram" => "deepgramApiKey",
            "elevenLabs" => "elevenLabsApiKey",
            "groq" => "groqApiKey",
            "openai" => "openaiApiKey",
            _ => {
                return Err(sqlx::Error::Protocol(
                    format!("Invalid provider: {}", provider).into(),
                ))
            }
        };

        let query = format!(
            "SELECT {} FROM transcript_settings WHERE id = '1' LIMIT 1",
            api_key_column
        );
        let api_key = sqlx::query_scalar(&query).fetch_optional(pool).await?;
        Ok(api_key)
    }

    pub async fn delete_api_key(
        pool: &SqlitePool,
        provider: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        let api_key_column = match provider {
            "openai" => "openaiApiKey",
            "ollama" => "ollamaApiKey",
            "groq" => "groqApiKey",
            "claude" => "anthropicApiKey",
            "openrouter" => "openRouterApiKey",
            "builtin-ai" => return Ok(()), // No API key needed
            _ => {
                return Err(sqlx::Error::Protocol(
                    format!("Invalid provider: {}", provider).into(),
                ))
            }
        };

        let query = format!(
            "UPDATE settings SET {} = NULL WHERE id = '1'",
            api_key_column
        );
        sqlx::query(&query).execute(pool).await?;

        Ok(())
    }

    // ===== CUSTOM OPENAI CONFIG METHODS =====

    /// Gets the custom OpenAI configuration from JSON
    ///
    /// # Returns
    /// * `Ok(Some(CustomOpenAIConfig))` - Config exists and is valid JSON
    /// * `Ok(None)` - No config stored
    /// * `Err(sqlx::Error)` - Database error
    pub async fn get_custom_openai_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<CustomOpenAIConfig>, sqlx::Error> {
        use sqlx::Row;

        let row = sqlx::query(
            r#"
            SELECT customOpenAIConfig
            FROM settings
            WHERE id = '1'
            LIMIT 1
            "#
        )
        .fetch_optional(pool)
        .await?;

        match row {
            Some(record) => {
                let config_json: Option<String> = record.get("customOpenAIConfig");

                if let Some(json) = config_json {
                    // Parse JSON into CustomOpenAIConfig
                    let config: CustomOpenAIConfig = serde_json::from_str(&json)
                        .map_err(|e| sqlx::Error::Protocol(
                            format!("Invalid JSON in customOpenAIConfig: {}", e).into()
                        ))?;

                    Ok(Some(config))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// Saves the custom OpenAI configuration as JSON
    ///
    /// # Arguments
    /// * `pool` - Database connection pool
    /// * `config` - CustomOpenAIConfig to save (includes endpoint, apiKey, model, maxTokens, temperature, topP)
    ///
    /// # Returns
    /// * `Ok(())` - Config saved successfully
    /// * `Err(sqlx::Error)` - Database or JSON serialization error
    pub async fn save_custom_openai_config(
        pool: &SqlitePool,
        config: &CustomOpenAIConfig,
    ) -> std::result::Result<(), sqlx::Error> {
        // Serialize config to JSON
        let config_json = serde_json::to_string(config)
            .map_err(|e| sqlx::Error::Protocol(
                format!("Failed to serialize config to JSON: {}", e).into()
            ))?;

        // Upsert into settings table
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, customOpenAIConfig)
            VALUES ('1', 'custom-openai', $1, 'large-v3', $2)
            ON CONFLICT(id) DO UPDATE SET
                customOpenAIConfig = excluded.customOpenAIConfig
            "#,
        )
        .bind(&config.model)
        .bind(config_json)
        .execute(pool)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod transcript_settings_tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn funasr_config_round_trips_without_an_api_key() {
        let pool = test_pool().await;
        SettingsRepository::save_transcript_config(
            &pool,
            "funasr",
            crate::config::DEFAULT_FUNASR_MODEL,
        )
        .await
        .unwrap();

        // The settings UI can still send a key value for a local provider.
        SettingsRepository::save_transcript_api_key(&pool, "funasr", "unused")
            .await
            .unwrap();

        let saved = SettingsRepository::get_transcript_config(&pool)
            .await
            .unwrap()
            .expect("FUN-ASR config should be readable");
        assert_eq!(saved.provider, "funasr");
        assert_eq!(saved.model, crate::config::DEFAULT_FUNASR_MODEL);
        assert_eq!(
            SettingsRepository::get_transcript_api_key(&pool, "funasr")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn remote_api_keys_are_still_stored() {
        let pool = test_pool().await;
        SettingsRepository::save_transcript_config(&pool, "deepgram", "nova-2")
            .await
            .unwrap();
        SettingsRepository::save_transcript_api_key(&pool, "deepgram", "secret")
            .await
            .unwrap();

        assert_eq!(
            SettingsRepository::get_transcript_api_key(&pool, "deepgram")
                .await
                .unwrap(),
            Some("secret".to_string())
        );
    }

    #[tokio::test]
    async fn summary_key_removal_preserves_other_settings_and_transcription_keys() {
        let pool = test_pool().await;
        SettingsRepository::save_model_config(&pool, "openai", "selected-model", "large-v3", None)
            .await
            .unwrap();
        let providers = ["openai", "claude", "groq", "openrouter"];
        for provider in providers {
            SettingsRepository::save_api_key(&pool, provider, &format!("{provider}-test-key"))
                .await
                .unwrap();
        }
        SettingsRepository::save_transcript_config(&pool, "openai", "transcript-model")
            .await
            .unwrap();
        SettingsRepository::save_transcript_api_key(&pool, "openai", "transcript-test-key")
            .await
            .unwrap();

        for (index, provider) in providers.iter().enumerate() {
            SettingsRepository::delete_api_key(&pool, provider)
                .await
                .unwrap();
            assert_eq!(
                SettingsRepository::get_api_key(&pool, provider)
                    .await
                    .unwrap(),
                None
            );
            for remaining in &providers[index + 1..] {
                assert_eq!(
                    SettingsRepository::get_api_key(&pool, remaining)
                        .await
                        .unwrap(),
                    Some(format!("{remaining}-test-key"))
                );
            }
        }
        let config = SettingsRepository::get_model_config(&pool)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(config.provider, "openai");
        assert_eq!(config.model, "selected-model");
        assert_eq!(
            SettingsRepository::get_transcript_api_key(&pool, "openai")
                .await
                .unwrap(),
            Some("transcript-test-key".to_string())
        );
    }

    #[tokio::test]
    async fn summary_key_lookup_distinguishes_missing_null_and_empty_values() {
        let pool = test_pool().await;
        assert_eq!(
            SettingsRepository::get_api_key(&pool, "openai")
                .await
                .unwrap(),
            None
        );
        SettingsRepository::save_model_config(&pool, "openai", "selected-model", "large-v3", None)
            .await
            .unwrap();
        assert_eq!(
            SettingsRepository::get_api_key(&pool, "openai")
                .await
                .unwrap(),
            None
        );
        SettingsRepository::save_api_key(&pool, "openai", "")
            .await
            .unwrap();
        assert_eq!(
            SettingsRepository::get_api_key(&pool, "openai")
                .await
                .unwrap(),
            Some(String::new())
        );
    }

    #[tokio::test]
    async fn unsupported_provider_does_not_delete_custom_endpoint_settings() {
        let pool = test_pool().await;
        SettingsRepository::save_model_config(&pool, "openai", "selected-model", "large-v3", None)
            .await
            .unwrap();
        let custom = r#"{"endpoint":"http://localhost","apiKey":"test-key","model":"model"}"#;
        sqlx::query("UPDATE settings SET customOpenAIConfig = $1 WHERE id = '1'")
            .bind(custom)
            .execute(&pool)
            .await
            .unwrap();

        assert!(SettingsRepository::delete_api_key(&pool, "custom-openai")
            .await
            .is_err());
        assert!(SettingsRepository::delete_api_key(&pool, "unknown")
            .await
            .is_err());
        let saved: String =
            sqlx::query_scalar("SELECT customOpenAIConfig FROM settings WHERE id = '1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(saved, custom);
    }
}
