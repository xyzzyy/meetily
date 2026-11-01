//! Model discovery for OpenAI-compatible local servers, including LM Studio.

#[derive(serde::Deserialize)]
struct ModelList {
    data: Vec<Model>,
}

#[derive(serde::Deserialize)]
struct Model {
    id: String,
}

#[tauri::command]
pub async fn get_custom_openai_models(
    endpoint: String,
    api_key: Option<String>,
) -> Result<Vec<String>, String> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err("Endpoint must start with http:// or https://".into());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Cannot create HTTP client: {e}"))?;
    let mut request = client.get(format!("{endpoint}/models"));
    if let Some(key) = api_key.filter(|key| !key.trim().is_empty()) {
        request = request.bearer_auth(key.trim());
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("Cannot connect to server: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Cannot fetch models: HTTP {}", response.status()));
    }
    let list = response
        .json::<ModelList>()
        .await
        .map_err(|e| format!("Invalid model list: {e}"))?;
    Ok(list
        .data
        .into_iter()
        .map(|model| model.id)
        .filter(|id| !id.trim().is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn server(status: &str, body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/", listener.local_addr().unwrap());
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 4096];
            let size = stream.read(&mut bytes).await.unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(bytes[..size].to_vec()).unwrap()
        });
        (endpoint, task)
    }

    #[tokio::test]
    async fn discovers_models_without_a_key() {
        let (endpoint, task) = server("200 OK", r#"{"data":[{"id":"local-mlx"},{"id":""}]}"#).await;
        assert_eq!(
            get_custom_openai_models(endpoint, None).await.unwrap(),
            vec!["local-mlx"]
        );
        let request = task.await.unwrap().to_lowercase();
        assert!(request.starts_with("get /v1/models http/1.1"));
        assert!(!request.contains("authorization:"));
    }

    #[tokio::test]
    async fn sends_optional_key_and_reports_http_errors() {
        let (endpoint, task) = server("401 Unauthorized", "{}").await;
        assert!(get_custom_openai_models(endpoint, Some(" token ".into()))
            .await
            .unwrap_err()
            .contains("401"));
        assert!(task
            .await
            .unwrap()
            .to_lowercase()
            .contains("authorization: bearer token"));
    }

    #[tokio::test]
    async fn handles_empty_and_invalid_lists() {
        for (body, valid) in [(r#"{"data":[]}"#, true), ("{}", false), ("not json", false)] {
            let (endpoint, task) = server("200 OK", body).await;
            assert_eq!(
                get_custom_openai_models(endpoint, None).await.is_ok(),
                valid
            );
            task.await.unwrap();
        }
        assert!(get_custom_openai_models("file:///tmp/models".into(), None)
            .await
            .is_err());
    }
}
