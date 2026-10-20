use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use tokio::{sync::mpsc, task::JoinHandle};

use super::ffmpeg::find_ffmpeg_path;

const CAPTURE_SAMPLE_RATE: u32 = 48_000;
const DIARIZATION_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug)]
pub struct StemWindow {
    pub microphone: Vec<f32>,
    pub system: Vec<f32>,
}

pub struct DiarizationStemSaver {
    sender: Option<mpsc::UnboundedSender<StemWindow>>,
    task: Option<JoinHandle<Result<()>>>,
    output_dir: PathBuf,
}

impl DiarizationStemSaver {
    pub fn start(meeting_folder: &Path, save_audio: bool) -> Result<Option<Self>> {
        // Speaker labels must not override the user's audio retention choice.
        if !save_audio {
            return Ok(None);
        }
        let output_dir = meeting_folder.join(".diarization");
        fs::create_dir_all(&output_dir)?;
        let mic_raw = output_dir.join(".microphone.f32");
        let system_raw = output_dir.join(".system.f32");
        let (sender, mut receiver) = mpsc::unbounded_channel::<StemWindow>();

        let task = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut microphone = BufWriter::new(File::create(&mic_raw)?);
            let mut system = BufWriter::new(File::create(&system_raw)?);
            while let Some(window) = receiver.blocking_recv() {
                write_f32_le(&mut microphone, &window.microphone)?;
                write_f32_le(&mut system, &window.system)?;
            }
            microphone.flush()?;
            system.flush()?;
            Ok(())
        });

        Ok(Some(Self {
            sender: Some(sender),
            task: Some(task),
            output_dir,
        }))
    }

    pub fn sender(&self) -> Option<mpsc::UnboundedSender<StemWindow>> {
        self.sender.clone()
    }

    pub async fn finalize(&mut self) -> Result<()> {
        self.sender = None;
        if let Some(task) = self.task.take() {
            task.await.context("Diarization stem writer crashed")??;
        }

        // Encoding hours of audio blocks, so keep it off the async runtime.
        let output_dir = self.output_dir.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let ffmpeg = find_ffmpeg_path().ok_or_else(|| anyhow!("FFmpeg is unavailable"))?;
            for stem in ["microphone", "system"] {
                let raw = output_dir.join(format!(".{stem}.f32"));
                let final_path = output_dir.join(format!("{stem}.flac"));
                let temporary = output_dir.join(format!(".{stem}.flac.tmp"));
                if !raw.exists() || raw.metadata()?.len() == 0 {
                    warn!("Skipping empty diarization stem: {}", raw.display());
                    let _ = fs::remove_file(raw);
                    continue;
                }

                let output = Command::new(&ffmpeg)
                    .args(["-y", "-hide_banner", "-loglevel", "error"])
                    .args([
                        "-f",
                        "f32le",
                        "-ar",
                        &CAPTURE_SAMPLE_RATE.to_string(),
                        "-ac",
                        "1",
                    ])
                    .arg("-i")
                    .arg(&raw)
                    .args(["-ar", &DIARIZATION_SAMPLE_RATE.to_string(), "-ac", "1"])
                    .args(["-c:a", "flac", "-compression_level", "5"])
                    .args(["-f", "flac"])
                    .arg(&temporary)
                    .output()?;
                if !output.status.success() {
                    return Err(anyhow!(
                        "Failed to finalize {stem} stem: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
                if !temporary.exists() || temporary.metadata()?.len() == 0 {
                    return Err(anyhow!("FFmpeg produced an empty {stem} stem"));
                }
                fs::rename(&temporary, &final_path)?;
                fs::remove_file(&raw)?;
                info!("Finalized diarization stem: {}", final_path.display());
            }
            Ok(())
        })
        .await
        .context("Diarization stem encoder crashed")?
    }
}

fn write_f32_le(writer: &mut impl Write, samples: &[f32]) -> Result<()> {
    for sample in samples {
        writer.write_all(&sample.to_le_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabling_audio_saving_creates_no_stem_files() {
        let directory = tempfile::tempdir().unwrap();
        let meeting = directory.path().join("meeting");
        assert!(DiarizationStemSaver::start(&meeting, false).unwrap().is_none());
        assert!(!meeting.exists());
    }
}
