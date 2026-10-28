use super::ffmpeg::find_ffmpeg_path; // Correct path to encode module
use super::AudioDevice;
use anyhow::Context;
use std::io::Write;
use std::sync::Arc;
use std::{
    path::PathBuf,
    process::{Command, Stdio},
};
use tracing::{debug, error};

pub struct AudioInput {
    pub data: Arc<Vec<f32>>,
    pub sample_rate: u32,
    pub channels: u16,
    pub device: Arc<AudioDevice>,
}

/// Encode mono or multichannel PCM samples as AAC in an MP4 file. Return
/// process and write errors, and wait for the encoder before returning.
pub fn encode_single_audio(
    data: &[u8],
    sample_rate: u32,
    channels: u16,
    output_path: &PathBuf,
) -> anyhow::Result<()> {
    debug!(
        "Starting FFmpeg process for {} bytes of audio data",
        data.len()
    );

    if data.is_empty() {
        return Err(anyhow::anyhow!("No audio data provided for encoding"));
    }

    let ffmpeg_path = find_ffmpeg_path().ok_or_else(|| {
        anyhow::anyhow!("FFmpeg not found. Please install FFmpeg to save recordings.")
    })?;

    debug!("Using FFmpeg at: {:?}", ffmpeg_path);

    let mut command = Command::new(ffmpeg_path);
    command
        .args([
            // Keep checkpoint logs small while retaining encoder warnings.
            "-loglevel",
            "warning",
            "-nostats",
            "-f",
            "f32le",
            "-ar",
            &sample_rate.to_string(),
            "-ac",
            &channels.to_string(),
            "-i",
            "pipe:0",
            "-c:a",
            "aac",
            "-b:a",
            "192k", // Increased from 64k for better audio quality (especially for speech)
            "-profile:a",
            "aac_low", // Use AAC-LC profile for better compatibility
            "-movflags",
            "+faststart", // Optimize for web streaming
            "-f",
            "mp4",
            output_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Hide console window on Windows to prevent CMD popup during recording
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    debug!("FFmpeg command: {:?}", command);

    run_encoder(command, data)
}

fn run_encoder(mut command: Command, data: &[u8]) -> anyhow::Result<()> {
    let mut ffmpeg = command.spawn().context("Failed to start FFmpeg")?;
    let stdin = ffmpeg.stdin.take();

    // Read both output pipes while the writer sends audio. Close stdin before
    // joining so that the encoder can read EOF and finish. Reap the child even
    // when it closes stdin early after an encoding error.
    let (output, write_result) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            if let Some(mut stdin) = stdin {
                stdin.write_all(data)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "FFmpeg input pipe is missing",
                ))
            }
        });
        let output = ffmpeg.wait_with_output();
        let write_result = writer
            .join()
            .map_err(|_| anyhow::anyhow!("FFmpeg input writer panicked"))?;
        Ok::<_, anyhow::Error>((output.context("Failed to wait for FFmpeg")?, write_result))
    })?;
    let status = output.status;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    debug!("FFmpeg process exited with status: {}", status);
    debug!("FFmpeg stdout: {}", stdout);
    debug!("FFmpeg stderr: {}", stderr);

    if !status.success() {
        error!("FFmpeg process failed with status: {}", status);
        error!("FFmpeg stderr: {}", stderr);
        return Err(anyhow::anyhow!(
            "FFmpeg process failed with status: {}: {}",
            status,
            stderr
        ));
    }

    write_result.context("Failed to send audio to FFmpeg")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const PIPE_BYTES: usize = 1024 * 1024;
    const FIXTURE_ENV: &str = "MEETILY_ENCODER_PIPE_TEST";

    // Run this test in a child process to supply encoder behavior without
    // Python, a shell, audio hardware, or an FFmpeg installation.
    #[test]
    fn encoder_process_fixture() {
        let Ok(mode) = std::env::var(FIXTURE_ENV) else {
            return;
        };
        // Fail a pipe-order regression within ten seconds instead of leaving
        // the test runner waiting for a blocked child.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(10));
            std::process::exit(124);
        });
        match mode.as_str() {
            "noisy" => {
                std::io::stdout()
                    .write_all(&vec![b'o'; PIPE_BYTES])
                    .unwrap();
                std::io::stderr()
                    .write_all(&vec![b'e'; PIPE_BYTES])
                    .unwrap();
                let mut data = Vec::new();
                std::io::stdin().read_to_end(&mut data).unwrap();
                assert_eq!(data, vec![7; PIPE_BYTES]);
            }
            "failure" => {
                std::io::stderr()
                    .write_all(b"encoder output path failed")
                    .unwrap();
                std::process::exit(1);
            }
            "early-success" => std::process::exit(0),
            _ => panic!("Unknown encoder fixture mode"),
        }
    }

    fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "audio::encode::tests::encoder_process_fixture",
                "--nocapture",
            ])
            .env(FIXTURE_ENV, mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    #[test]
    fn drains_output_while_writing_audio() {
        run_encoder(fixture_command("noisy"), &vec![7; PIPE_BYTES]).unwrap();
    }

    #[test]
    fn preserves_encoder_error_after_input_pipe_closes() {
        let error = run_encoder(fixture_command("failure"), &vec![7; PIPE_BYTES]).unwrap_err();
        assert!(
            error.to_string().contains("encoder output path failed"),
            "{error:#}"
        );
    }

    #[test]
    fn reports_incomplete_input_even_if_encoder_exits_successfully() {
        let error =
            run_encoder(fixture_command("early-success"), &vec![7; PIPE_BYTES]).unwrap_err();
        assert!(
            error.to_string().contains("Failed to send audio"),
            "{error:#}"
        );
    }
}
