//! Safe messages for failures from user-configured HTTP endpoints.
//!
//! Request errors can include URLs, and server responses can echo credentials.
//! Describe the failure without formatting the error or copying response text.

pub(crate) fn request_error(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "The request timed out. Check the server and try again."
    } else if error.is_connect() {
        "Cannot connect to the server. Check the endpoint settings and server status."
    } else if error.is_builder() {
        "Could not build the request. Check the endpoint and request settings."
    } else if error.is_decode() {
        "The server response could not be decoded."
    } else if error.is_redirect() {
        "The server returned an invalid redirect. Check the endpoint settings."
    } else if error.is_body() {
        "Could not transfer the request or response body. Try again."
    } else {
        "The HTTP request failed. Check the server and try again."
    }
}

pub(crate) fn status_error(status: reqwest::StatusCode) -> String {
    let detail = match status.as_u16() {
        400 => "The server rejected the request. Check the model and request settings.",
        401 => "Authentication failed. Check the API key or endpoint credentials.",
        403 => "The server denied access. Check the account permissions.",
        404 => "The requested resource was not found. Check the model and endpoint settings.",
        429 => "The server rate limit was reached. Try again later.",
        500..=599 => "The server could not complete the request. Try again later.",
        _ => "The server rejected the request.",
    };
    format!("HTTP {}: {detail}", status.as_u16())
}

#[cfg(test)]
pub(crate) mod test_support {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) fn assert_private(message: &str) {
        for secret in [
            "review-user",
            "review-password",
            "review-query-secret",
            "review-header-secret",
        ] {
            assert!(
                !message.contains(secret),
                "credential reached an error message: {message}"
            );
        }
        assert!(
            !message.contains("http://"),
            "request URL reached an error message: {message}"
        );
    }

    pub(crate) fn closed_endpoint() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://review-user:review-password@{address}/v1?api_key=review-query-secret")
    }

    pub(crate) async fn respond_once(
        status: &str,
        body: &str,
    ) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        );
        let server = tokio::spawn(async move {
            let (mut stream, _) =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let length = String::from_utf8_lossy(&request[..end])
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            request
        });
        (
            format!("http://review-user:review-password@{address}/v1?api_key=review-query-secret"),
            server,
        )
    }
}
