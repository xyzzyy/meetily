# Nemotron 3.5 Transcribe

Meetily downloads a private Python 3.12.12 runtime and MLX dependencies when the user downloads this model in Settings. No system Python install is needed. The worker runs the full 30B-A4B model with a 4-bit language model on Apple silicon. The speech and speaker encoders keep their original precision. Windows, Linux, and Intel Macs are not supported by this MLX runtime.

The model download is 20,552 MB (about 19 GiB): 19,275 MB for Nemotron and 1,277 MB for the word aligner. Allow more space for Python, dependencies, and download files. Use at least 32 GB of unified memory. This is a memory recommendation, not a measured Meetily performance limit. Inference can be slower than real time.

The worker loads the local model with `strict=True`. Hugging Face and Transformers offline modes are set during inference. Audio stays on the device. Meetily sends mono 16 kHz samples to the worker over standard input. The worker returns plain transcript text over standard output. It detects the spoken language. It does not enable translation or model speaker labels.

Nemotron has no word timestamps. When Enhance asks for them, the worker runs Qwen3-ForcedAligner 0.6B on the same audio and Nemotron's transcript, then puts each time on Nemotron's own words, so punctuation is kept. The aligner loads on the first such request. Enhance can then send Nemotron sentence-length audio and still split rows where the speaker changes. On short clips Nemotron drops phrases and guesses the language. Live transcription does not request word times.

The Rust installer downloads uv 0.11.18 and checks its SHA-256. uv installs a managed Python and a private virtual environment, and it checks the pinned hash of the MLX source archive. `download.py` then downloads the model and aligner files with `huggingface_hub` at a fixed commit revision, and the installer checks each file's SHA-256. `huggingface_hub` retries and resumes after network errors within one download. Cancel stops the download and removes unfinished files. Retry keeps completed files and downloads unfinished files again from the start. Python dependencies use the tested versions in `constraints.txt`. All installed files are in the branch's owned model directory. Delete removes both the model and the private runtime. A model in use cannot be deleted. Cancel during a runtime install step takes effect after that step.

Sources:

- [NVIDIA model](https://huggingface.co/nvidia/NVIDIA-Nemotron-3.5-Transcribe-30B-A4B): original model, architecture, and license.
- [MLX export](https://huggingface.co/nativ-community/Nemotron-3.5-Transcribe-30B-A4B-EA-4bit/tree/c47153ca2ce4024d43e216b81e6db4f67b886545): pinned full-model export and its published validation notes.
- [MLX implementation](https://github.com/Blaizzy/mlx-audio/tree/92decb7577878ef8e9274c1a7e33084760be03aa/mlx_audio/stt/models/nemotron_transcribe): pinned inference code.
- [Word aligner](https://huggingface.co/mlx-community/Qwen3-ForcedAligner-0.6B-8bit/tree/0e1a68e91d815300c7c9754b2a7639378b23db15): pinned MLX export of Qwen3-ForcedAligner 0.6B (Apache 2.0).
- [uv release](https://github.com/astral-sh/uv/releases/tag/0.11.18): runtime installer archive and digest.

Run the helper tests with `python3 -m unittest discover -s nemotron-helper -p 'test_*.py'`. These tests use a model stub. A full inference test needs the model download and an Apple silicon Mac with enough free memory.
