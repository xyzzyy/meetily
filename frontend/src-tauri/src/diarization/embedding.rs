// diarization/embedding.rs
//
// Speaker-embedding extraction using the WeSpeaker CAM++ ONNX model
// (input: kaldi fbank features [1, T, 80], output: 512-dim embedding).

use super::fbank::{FbankComputer, NUM_MEL_BINS};
use ndarray::Array3;
use ort::execution_providers::CPUExecutionProvider;
use ort::inputs;
use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::session::Session;
use ort::value::TensorRef;
use std::path::Path;

#[derive(thiserror::Error, Debug)]
pub enum EmbeddingError {
    #[error("ONNX Runtime error: {0}")]
    Ort(#[from] ort::Error),
    #[error("Audio too short for embedding extraction")]
    AudioTooShort,
    #[error("Model produced no embedding output")]
    NoOutput,
}

pub struct EmbeddingExtractor {
    session: Session,
    input_name: String,
    output_name: String,
    fbank: FbankComputer,
}

// ORT 1.22 Pad_Fusion changes ceil-mode AveragePool boundary divisors in
// this model. Keep the explicit Pad node so it uses the correct exclude-pad
// path. Upstream pooling fix: microsoft/onnxruntime#29629.
fn embedding_session_builder() -> Result<SessionBuilder, ort::Error> {
    Session::builder()?
        .with_execution_providers(vec![CPUExecutionProvider::default().build()])?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_disabled_optimizers("Pad_Fusion")?
        .with_intra_threads(2)
}

impl EmbeddingExtractor {
    pub fn new(model_path: &Path) -> Result<Self, EmbeddingError> {
        let session = embedding_session_builder()?.commit_from_file(model_path)?;

        // Resolve I/O names dynamically ("feats"/"embs" for WeSpeaker exports)
        let input_name = session
            .inputs
            .first()
            .map(|i| i.name.clone())
            .unwrap_or_else(|| "feats".to_string());
        let output_name = session
            .outputs
            .first()
            .map(|o| o.name.clone())
            .unwrap_or_else(|| "embs".to_string());

        log::info!(
            "Diarization embedding model loaded from {} (input: {}, output: {})",
            model_path.display(),
            input_name,
            output_name
        );

        Ok(Self {
            session,
            input_name,
            output_name,
            fbank: FbankComputer::new(),
        })
    }

    /// Compute an L2-normalized speaker embedding from 16kHz mono f32 samples.
    pub fn compute(&mut self, samples_16k: &[f32]) -> Result<Vec<f32>, EmbeddingError> {
        let features = self.fbank.compute(samples_16k);
        if features.len() < 10 {
            // Fewer than ~100ms of frames produces meaningless embeddings
            return Err(EmbeddingError::AudioTooShort);
        }

        let num_frames = features.len();
        let mut feats = Array3::<f32>::zeros((1, num_frames, NUM_MEL_BINS));
        for (t, frame) in features.iter().enumerate() {
            for (f, &v) in frame.iter().enumerate() {
                feats[[0, t, f]] = v;
            }
        }

        let inputs = inputs![
            self.input_name.as_str() => TensorRef::from_array_view(feats.view())?,
        ];
        let outputs = self.session.run(inputs)?;

        let embedding = outputs
            .get(self.output_name.as_str())
            .ok_or(EmbeddingError::NoOutput)?
            .try_extract_array::<f32>()?;

        let mut vec: Vec<f32> = embedding.iter().copied().collect();
        let norm = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut vec {
                *v /= norm;
            }
        }
        Ok(vec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceil_mode_pooling_keeps_the_partial_window_divisor() {
        // Opset 14: explicit zero Pad -> AveragePool(k=100, stride=100,
        // ceil_mode=1). The last window has 49 real samples. Pad_Fusion in
        // ORT 1.22 changes its divisor to 100, corrupting CAM++ features.
        let model = include_bytes!("../../tests/fixtures/ceil_mode_pool.onnx");
        let mut session = embedding_session_builder()
            .unwrap()
            .commit_from_memory(model)
            .unwrap();
        let values: Vec<f32> = (1..=349).map(|n| n as f32).collect();
        let outputs = session
            .run(inputs!["input" =>
                ort::value::Tensor::from_array(([1usize, 1, 349], values)).unwrap()
            ])
            .unwrap();
        let (_, result) = outputs["output"].try_extract_tensor::<f32>().unwrap();
        assert_eq!(result, &[50.5, 150.5, 250.5, 325.0]);
    }
}
