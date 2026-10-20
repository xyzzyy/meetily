use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

const REQUEST_MAGIC: &[u8; 4] = b"MFAS";
const RESPONSE_MAGIC: &[u8; 4] = b"MFAR";
const PROTOCOL_VERSION: u16 = 1;
const OP_TRANSCRIBE: u16 = 1;
const OP_PING: u16 = 2;
const OP_SHUTDOWN: u16 = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// Longest audio the helper accepts in one request (16 kHz samples).
pub(crate) const MAX_AUDIO_SAMPLES: usize = 30 * 16_000;

#[derive(Debug)]
pub struct SidecarRequestError {
    message: String,
    pub response_received: bool,
}

impl SidecarRequestError {
    fn new(error: impl std::fmt::Display, response_received: bool) -> Self {
        Self {
            message: error.to_string(),
            response_received,
        }
    }
}

impl std::fmt::Display for SidecarRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SidecarRequestError {}

pub struct FunAsrSidecar {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    next_request_id: u64,
}

impl FunAsrSidecar {
    pub async fn spawn(encoder: &Path, decoder: &Path) -> Result<Self> {
        let helper = resolve_helper_binary()?;
        log::info!(
            "Starting FUN-ASR helper {} with encoder {} and decoder {}",
            helper.display(),
            encoder.display(),
            decoder.display()
        );

        let mut child = Command::new(&helper)
            .arg("--encoder")
            .arg(encoder)
            .arg("--decoder")
            .arg(decoder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("Failed to start FUN-ASR helper at {}", helper.display()))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("FUN-ASR helper stdin was not available"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("FUN-ASR helper stdout was not available"))?;

        let mut sidecar = Self {
            child,
            stdin,
            stdout,
            next_request_id: 1,
        };
        let pong = sidecar
            .request(OP_PING, "auto", &[])
            .await
            .map_err(anyhow::Error::new)?;
        if pong != "pong" {
            return Err(anyhow!(
                "FUN-ASR helper returned an invalid startup response"
            ));
        }
        Ok(sidecar)
    }

    pub async fn transcribe(
        &mut self,
        language: &str,
        audio: &[f32],
    ) -> std::result::Result<String, SidecarRequestError> {
        self.request(OP_TRANSCRIBE, language, audio).await
    }

    #[cfg(test)]
    pub(super) fn process_id(&self) -> Option<u32> {
        self.child.id()
    }

    #[cfg(test)]
    pub(super) async fn terminate_for_test(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    pub async fn shutdown(&mut self) {
        let _ = timeout(
            Duration::from_secs(2),
            self.request(OP_SHUTDOWN, "auto", &[]),
        )
        .await;
        if timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }

    async fn request(
        &mut self,
        opcode: u16,
        language: &str,
        audio: &[f32],
    ) -> std::result::Result<String, SidecarRequestError> {
        self.request_with_timeout(opcode, language, audio, REQUEST_TIMEOUT)
            .await
    }

    async fn request_with_timeout(
        &mut self,
        opcode: u16,
        language: &str,
        audio: &[f32],
        request_timeout: Duration,
    ) -> std::result::Result<String, SidecarRequestError> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        let request = build_request(request_id, opcode, language, audio)?;

        let mut response_received = false;
        let request_result: Result<String> = timeout(request_timeout, async {
            self.stdin.write_all(&request).await?;
            self.stdin.flush().await?;

            let mut header = [0u8; 24];
            self.stdout.read_exact(&mut header).await?;
            response_received = true;
            if &header[0..4] != RESPONSE_MAGIC {
                return Err(anyhow!("FUN-ASR helper returned invalid response magic"));
            }
            let version = u16::from_le_bytes([header[4], header[5]]);
            if version != PROTOCOL_VERSION {
                return Err(anyhow!(
                    "Unsupported FUN-ASR helper protocol version {version}"
                ));
            }
            let status = u16::from_le_bytes([header[6], header[7]]);
            let response_id = u64::from_le_bytes(header[8..16].try_into().unwrap());
            if response_id != request_id {
                return Err(anyhow!(
                    "FUN-ASR helper response ID mismatch: expected {request_id}, got {response_id}"
                ));
            }
            let text_len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
            let error_len = u32::from_le_bytes(header[20..24].try_into().unwrap()) as usize;
            if text_len > MAX_TEXT_BYTES || error_len > MAX_TEXT_BYTES {
                return Err(anyhow!("FUN-ASR helper response exceeded the size limit"));
            }

            let mut text = vec![0u8; text_len];
            let mut error = vec![0u8; error_len];
            self.stdout.read_exact(&mut text).await?;
            self.stdout.read_exact(&mut error).await?;

            let text = String::from_utf8(text)
                .context("FUN-ASR helper returned non-UTF-8 transcription text")?;
            let error =
                String::from_utf8(error).context("FUN-ASR helper returned a non-UTF-8 error")?;
            if status != 0 {
                return Err(anyhow!(
                    "{}",
                    if error.is_empty() {
                        "FUN-ASR helper failed".to_string()
                    } else {
                        error
                    }
                ));
            }
            Ok(text)
        })
        .await
        .map_err(|_| anyhow!("FUN-ASR helper request timed out"))
        .and_then(|result| result);

        request_result.map_err(|error| SidecarRequestError::new(error, response_received))
    }
}

fn build_request(
    request_id: u64,
    opcode: u16,
    language: &str,
    audio: &[f32],
) -> std::result::Result<Vec<u8>, SidecarRequestError> {
    let language_bytes = language.as_bytes();
    if language_bytes.len() > u16::MAX as usize {
        return Err(SidecarRequestError::new("Language hint is too long", false));
    }
    if audio.len() > MAX_AUDIO_SAMPLES {
        return Err(SidecarRequestError::new(
            "Audio request exceeds the 30-second limit",
            false,
        ));
    }

    let mut request = Vec::with_capacity(26 + language_bytes.len() + audio.len() * 4);
    request.extend_from_slice(REQUEST_MAGIC);
    request.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    request.extend_from_slice(&opcode.to_le_bytes());
    request.extend_from_slice(&request_id.to_le_bytes());
    request.extend_from_slice(&16_000u32.to_le_bytes());
    request.extend_from_slice(&(language_bytes.len() as u16).to_le_bytes());
    request.extend_from_slice(&(audio.len() as u32).to_le_bytes());
    request.extend_from_slice(language_bytes);
    for sample in audio {
        request.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(request)
}

fn helper_filename() -> String {
    if cfg!(windows) {
        "funasr-helper.exe".to_string()
    } else {
        "funasr-helper".to_string()
    }
}

fn target_helper_filename() -> String {
    let triple = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    if cfg!(windows) {
        format!("funasr-helper-{triple}.exe")
    } else {
        format!("funasr-helper-{triple}")
    }
}

fn resolve_helper_binary() -> Result<PathBuf> {
    if let Ok(override_path) = std::env::var("MEETILY_FUNASR_HELPER") {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "MEETILY_FUNASR_HELPER does not point to a file: {}",
            path.display()
        ));
    }

    let plain = helper_filename();
    let target = target_helper_filename();
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(&plain));
            candidates.push(directory.join(&target));
            candidates.push(directory.join("binaries").join(&target));
            if let Some(resources) = directory.parent().map(|path| path.join("Resources")) {
                candidates.push(resources.join(&plain));
                candidates.push(resources.join(&target));
            }
        }
    }
    // Development builds and tests also look in the source tree. Release builds
    // only use the bundled helper, never a binary from the working directory.
    if cfg!(debug_assertions) {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        candidates.push(manifest.join("binaries").join(&target));
        candidates.push(manifest.join("../../funasr-helper/build/bin").join(&plain));
        if let Ok(current) = std::env::current_dir() {
            candidates.push(current.join("frontend/src-tauri/binaries").join(&target));
            candidates.push(current.join("funasr-helper/build/bin").join(&plain));
        }
    }

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            anyhow!(
                "funasr-helper was not found. Build it with frontend/scripts/build-funasr-sidecar.js or set MEETILY_FUNASR_HELPER"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(unix)]
    async fn sidecar_with_command(program: &str, arguments: &[&str]) -> FunAsrSidecar {
        let mut child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        FunAsrSidecar {
            child,
            stdin,
            stdout,
            next_request_id: 1,
        }
    }

    #[cfg(unix)]
    async fn sidecar_with_response(response: &[u8]) -> (tempfile::NamedTempFile, FunAsrSidecar) {
        let mut response_file = tempfile::NamedTempFile::new().unwrap();
        response_file.write_all(response).unwrap();
        response_file.flush().unwrap();
        let path = response_file.path().to_str().unwrap().to_string();
        let sidecar = sidecar_with_command("/bin/cat", &[&path]).await;
        (response_file, sidecar)
    }

    #[test]
    fn bundled_filename_contains_the_target_triple() {
        let name = target_helper_filename();
        assert!(name.starts_with("funasr-helper-"));
        assert!(name.contains("darwin") || name.contains("windows") || name.contains("linux"));
    }

    #[test]
    fn request_frame_is_versioned_and_little_endian() {
        let frame = build_request(42, OP_TRANSCRIBE, "ja", &[0.5, -0.25]).unwrap();
        assert_eq!(&frame[0..4], b"MFAS");
        assert_eq!(u16::from_le_bytes(frame[4..6].try_into().unwrap()), 1);
        assert_eq!(
            u16::from_le_bytes(frame[6..8].try_into().unwrap()),
            OP_TRANSCRIBE
        );
        assert_eq!(u64::from_le_bytes(frame[8..16].try_into().unwrap()), 42);
        assert_eq!(
            u32::from_le_bytes(frame[16..20].try_into().unwrap()),
            16_000
        );
        assert_eq!(u16::from_le_bytes(frame[20..22].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(frame[22..26].try_into().unwrap()), 2);
        assert_eq!(&frame[26..28], b"ja");
        assert_eq!(f32::from_le_bytes(frame[28..32].try_into().unwrap()), 0.5);
        assert_eq!(f32::from_le_bytes(frame[32..36].try_into().unwrap()), -0.25);
    }

    #[test]
    fn request_frame_rejects_oversized_audio() {
        let audio = vec![0.0; MAX_AUDIO_SAMPLES + 1];
        assert!(build_request(1, OP_TRANSCRIBE, "auto", &audio).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn malformed_response_is_not_safe_to_retry() {
        let (_response_file, mut sidecar) =
            sidecar_with_response(b"NOPE00000000000000000000").await;
        let error = sidecar.request(OP_TRANSCRIBE, "en", &[]).await.unwrap_err();
        assert!(error.response_received);
        assert!(error.to_string().contains("invalid response magic"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn helper_error_response_is_not_safe_to_retry() {
        let error_message = b"fixture failure";
        let mut response = Vec::new();
        response.extend_from_slice(RESPONSE_MAGIC);
        response.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        response.extend_from_slice(&1u16.to_le_bytes());
        response.extend_from_slice(&1u64.to_le_bytes());
        response.extend_from_slice(&0u32.to_le_bytes());
        response.extend_from_slice(&(error_message.len() as u32).to_le_bytes());
        response.extend_from_slice(error_message);
        let (_response_file, mut sidecar) = sidecar_with_response(&response).await;

        let error = sidecar.request(OP_TRANSCRIBE, "en", &[]).await.unwrap_err();
        assert!(error.response_received);
        assert_eq!(error.to_string(), "fixture failure");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn eof_before_a_response_is_safe_to_retry() {
        let mut sidecar = sidecar_with_command("/usr/bin/true", &[]).await;
        let error = sidecar.request(OP_TRANSCRIBE, "en", &[]).await.unwrap_err();
        assert!(!error.response_received);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn request_timeout_is_safe_to_retry() {
        let mut sidecar = sidecar_with_command("/bin/sleep", &["5"]).await;
        let error = sidecar
            .request_with_timeout(OP_TRANSCRIBE, "en", &[], Duration::from_millis(20))
            .await
            .unwrap_err();
        assert!(!error.response_received);
        assert!(error.to_string().contains("timed out"));
        sidecar.terminate_for_test().await;
    }
}
