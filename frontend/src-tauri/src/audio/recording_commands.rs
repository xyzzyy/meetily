// audio/recording_commands.rs
//
// Slim Tauri command layer for recording functionality.
// Delegates to transcription and recording modules for actual implementation.

use anyhow::Result;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::task::JoinHandle;

use super::{
    recording_manager::{RecordingStartError, RecordingStopOutcome},
    parse_audio_device,
    default_input_device,   // Get default microphone
    default_output_device,  // Get default system audio
    RecordingManager,
};
use super::device_monitor::{DeviceEvent, DeviceMonitorType};

// Import transcription modules
use super::transcription::{self, reset_speech_detected_flag};

// Re-export TranscriptUpdate for backward compatibility
pub use super::transcription::TranscriptUpdate;

// ============================================================================
// GLOBAL STATE
// ============================================================================

// Simple recording state tracking
static IS_RECORDING: AtomicBool = AtomicBool::new(false);

/// True for the whole `stop_recording` tail (manager taken -> `recording-stopped`
/// emitted). `IS_RECORDING` must stay true through the tail — the frontend polls
/// it to keep the stop UI up — so the mic-disconnect fallback checks this flag
/// too, otherwise a fallback queued before Stop retries against a taken manager
/// and surfaces a spurious "Microphone fallback failed" toast.
static IS_RECORDING_STOPPING: AtomicBool = AtomicBool::new(false);

/// Recording is live and not being torn down — the only state in which the
/// mic-disconnect fallback should run or report.
fn recording_live() -> bool {
    IS_RECORDING.load(Ordering::SeqCst) && !IS_RECORDING_STOPPING.load(Ordering::SeqCst)
}

/// Recording is live AND the global manager is still the session `s` belongs to.
/// Used by the mic-disconnect fallback to refuse acting on a *later* recording
/// after a Stop/Start swapped the manager out from under an in-flight task.
///
/// NOTE: this locks `RECORDING_MANAGER`. Never call it while already holding
/// that lock (e.g. inside a `RECORDING_MANAGER.lock()` scope) — the std Mutex
/// is non-reentrant and it would self-deadlock. All current callers invoke it
/// outside any held lock; keep it that way.
fn session_live(s: &Arc<super::RecordingState>) -> bool {
    recording_live()
        && RECORDING_MANAGER
            .lock()
            .unwrap()
            .as_ref()
            .map_or(false, |m| Arc::ptr_eq(m.get_state(), s))
}

/// Guard for exclusive stop ownership. Sets the flag on acquisition and clears
/// it on Drop, including during unwind. A failed stop cannot leave the flag
/// set and disable microphone recovery for later recordings.
///
/// This unwind-clears behaviour depends on `panic = "unwind"` (the default).
/// If a release profile ever sets `panic = "abort"`, Drop won't run on panic
/// and the stuck-flag failure mode returns — add a start-time reset then.
struct StoppingGuard<'a> {
    flag: &'a AtomicBool,
}
impl<'a> StoppingGuard<'a> {
    /// Acquire stop ownership only if another stop does not already own it.
    fn try_acquire(flag: &'a AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self { flag })
    }
}
impl Drop for StoppingGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

/// Shared start-path finalize. Both start commands MUST call this so a new
/// start path can't silently ship with a per-session flag left unreset (e.g.
/// the mic-recovery budget already exhausted).
fn finalize_recording_start() {
    info!("🔍 Setting IS_RECORDING to true and resetting SPEECH_DETECTED_EMITTED");
    IS_RECORDING.store(true, Ordering::SeqCst);
    MIC_FALLBACK_FAILED_ATTEMPTS.store(0, Ordering::SeqCst); // fresh mic-recovery budget per session
    reset_speech_detected_flag(); // reset speech-detected emit latch for the new session
}

// Global recording manager and transcription task to keep them alive during recording
static RECORDING_MANAGER: Mutex<Option<RecordingManager>> = Mutex::new(None);
static TRANSCRIPTION_TASK: Mutex<Option<JoinHandle<Result<(), String>>>> = Mutex::new(None);

// Listener ID for proper cleanup - prevents microphone from staying active after recording stops
static TRANSCRIPT_LISTENER_ID: Mutex<Option<tauri::EventId>> = Mutex::new(None);

const TRANSCRIPTION_RUNTIME_START_ERROR_CODE: &str =
    "TRANSCRIPTION_RUNTIME_INITIALIZATION_FAILED";
const TRANSCRIPTION_RUNTIME_USER_MESSAGE: &str = "Speech recognition could not initialize. Restart Meetily. If the problem continues, repair or reinstall the app.";

// ============================================================================
// PUBLIC TYPES
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct RecordingArgs {
    pub save_path: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct TranscriptionStatus {
    pub chunks_in_queue: usize,
    pub is_processing: bool,
    pub last_activity_ms: u64,
}

fn map_recording_start_error<R: Runtime>(
    app: &AppHandle<R>,
    error: RecordingStartError,
) -> String {
    crate::tray::update_tray_menu(app);

    match error {
        RecordingStartError::TranscriptionRuntime(source) => {
            error!("Failed to initialize speech recognition: {source:#}");
            let error = RecordingStartError::TranscriptionRuntime(source);
            if let Err(emit_error) = app.emit("transcription-error", serde_json::json!({
                "error": error.to_string(),
                "userMessage": TRANSCRIPTION_RUNTIME_USER_MESSAGE,
                "actionable": false,
                "phase": "startup"
            })) {
                error!("Failed to emit transcription runtime startup error: {emit_error}");
            }
            TRANSCRIPTION_RUNTIME_START_ERROR_CODE.to_string()
        }
        RecordingStartError::Other(error) => format!("Failed to start recording: {error:#}"),
    }
}

// ============================================================================
// DEVICE RESOLUTION
// ============================================================================

/// Resolve the microphone to record with: requested device (if it actually
/// enumerates) → system default → none (system-audio-only recording).
///
/// The device picker has no "no microphone" option: choosing "Default
/// Microphone" sends `None`, so `None` here means "use the system default",
/// NOT "record without a mic". A specifically-requested mic that isn't in
/// cpal's current enumeration (a stale saved device, or a Continuity
/// "iPhone Microphone" that isn't available right now) is downgraded to the
/// system default — the same `default_input_device()` helper the
/// mid-recording disconnect path uses — so start never hard-fails with
/// "Device not found".
///
/// Emits at most one event per call:
/// - `mic-device-switched` — a specific mic was requested but unavailable,
///   and we fell back to the default (reuses the existing frontend listener).
/// - `mic-unavailable` — no usable mic at all; recording proceeds with
///   system audio only. If system audio is also unavailable, start_streams'
///   own guard reports it.
/// Resolving `None` to the default is the user's actual choice, so it's silent.
///
/// ponytail: sync pre-flight substitution (matches Pro), not catch-and-retry —
/// stream.rs keeps its hard-fail as the last line of defense. cpal calls
/// block briefly either way.
fn resolve_mic_or_default<R: Runtime>(
    app: &AppHandle<R>,
    requested_name: Option<&str>,
) -> Option<Arc<super::AudioDevice>> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let requested_specific = requested_name.is_some();

    if let Some(name) = requested_name {
        match parse_audio_device(name) {
            Ok(device) => {
                let exists = cpal::default_host()
                    .input_devices()
                    .map(|mut it| it.any(|d| d.name().map(|n| n == device.name).unwrap_or(false)))
                    .unwrap_or(false);
                if exists {
                    info!("✅ Using requested microphone: '{}'", device.name);
                    return Some(Arc::new(device));
                }
                warn!(
                    "⚠️ Requested mic '{}' not enumerated — falling back to system default",
                    device.name
                );
            }
            Err(e) => {
                warn!(
                    "⚠️ Requested mic '{}' not available: {} — falling back to system default",
                    name, e
                );
            }
        }
    }

    match default_input_device() {
        Ok(device) => {
            info!("✅ Using default microphone: '{}'", device.name);
            if requested_specific {
                // Tell the user their selected mic wasn't available and which
                // mic is actually recording. Reuses the mic-device-switched
                // listener the disconnect path wires up.
                let _ = app.emit(
                    "mic-device-switched",
                    serde_json::json!({ "device_name": device.name }),
                );
            }
            Some(Arc::new(device))
        }
        Err(e) => {
            warn!("❌ No microphone available: {} — recording system audio only", e);
            let _ = app.emit("mic-unavailable", serde_json::json!({}));
            None
        }
    }
}

/// System-audio analog of `resolve_mic_or_default`: `Some(name)` -> parse it,
/// falling back to the default output if unparseable; `None` ("Default System
/// Audio" in the UI) -> default output. Returns `None` only when no output
/// device exists — system audio is optional, mic-only recording proceeds.
///
/// ponytail: no cpal enumeration check (unlike the mic helper) — Linux system
/// devices are Pulse/ALSA monitor *inputs* tagged Output, so output_devices()
/// would false-negative them. stream.rs still hard-fails on a missing device.
fn resolve_system_or_default(requested_name: Option<&str>) -> Option<Arc<super::AudioDevice>> {
    if let Some(name) = requested_name {
        match parse_audio_device(name) {
            Ok(device) => {
                info!("✅ Using requested system audio: '{}'", device.name);
                return Some(Arc::new(device));
            }
            Err(e) => warn!(
                "⚠️ Requested system audio '{}' not available: {} — falling back to system default",
                name, e
            ),
        }
    }

    match default_output_device() {
        Ok(device) => {
            info!("✅ Using default system audio: '{}'", device.name);
            Some(Arc::new(device))
        }
        Err(e) => {
            warn!("⚠️ No system audio available: {} — recording will continue with microphone only", e);
            None
        }
    }
}

/// Wake idle audio hardware before checking microphone callbacks, and finish
/// validation before creating any recording resources.
#[cfg(target_os = "macos")]
async fn prepare_audio_for_recording(
    system_device: Option<&super::AudioDevice>,
) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let wake_name = system_device
        .map(|s| s.name.clone())
        .or_else(|| {
            cpal::default_host()
                .default_output_device()
                .and_then(|d| d.name().ok())
        });
    if let Some(name) = wake_name {
        if let Err(e) = super::recording_manager::wake_audio_connection(&name).await {
            warn!("[AUDIO_WAKE] Wake failed: {} — proceeding anyway", e);
        }
    }

    if let Err(e) = super::devices::verify_microphone_access().await {
        error!("Microphone access verification failed: {}", e);
        return Err(format!("Microphone access required: {}", e));
    }
    Ok(())
}

// ============================================================================
// RECORDING COMMANDS
// ============================================================================

/// Start recording with default devices
pub async fn start_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    start_recording_with_meeting_name(app, None).await
}

/// Start recording with default devices and optional meeting name
pub async fn start_recording_with_meeting_name<R: Runtime>(
    app: AppHandle<R>,
    meeting_name: Option<String>,
) -> Result<(), String> {
    info!(
        "Starting recording with default devices, meeting: {:?}",
        meeting_name
    );

    let engine_lifecycle_guard = super::common::acquire_engine_lifecycle_lock().await;

    // Check if already recording
    let current_recording_state = IS_RECORDING.load(Ordering::SeqCst);
    info!("🔍 IS_RECORDING state check: {}", current_recording_state);
    if current_recording_state {
        return Err("Recording already in progress".to_string());
    }

    if let Err(error) = crate::ensure_onnx_runtime_available() {
        return Err(map_recording_start_error(
            &app,
            RecordingStartError::TranscriptionRuntime(error),
        ));
    }

    // Validate that transcription models are available before starting recording
    info!("🔍 Validating transcription model availability before starting recording...");
    if let Err(validation_error) = transcription::validate_transcription_model_ready(&app).await {
        error!("Model validation failed: {}", validation_error);

        // Emit error event for frontend - actionable: false to show toast instead of modal
        // (download progress is already shown in top-right toast)
        let _ = app.emit("transcription-error", serde_json::json!({
            "error": validation_error,
            "userMessage": format!("Recording cannot start: {}", validation_error),
            "actionable": false,
            "phase": "startup"
        }));

        return Err(validation_error);
    }
    info!("✅ Transcription model validation passed");

    // Notify frontend that startup has begun (surfaces STARTING state)
    app.emit("recording-starting", serde_json::json!({
        "message": "Recording initialization started"
    })).map_err(|e| e.to_string())?;

    // Load recording preferences to get auto_save AND device preferences
    let (auto_save, preferred_mic_name, preferred_system_name) =
        match super::recording_preferences::load_recording_preferences(&app).await {
            Ok(prefs) => {
                info!("📋 Loaded recording preferences: auto_save={}, preferred_mic={:?}, preferred_system={:?}",
                      prefs.auto_save, prefs.preferred_mic_device, prefs.preferred_system_device);
                (
                    prefs.auto_save,
                    prefs.preferred_mic_device,
                    prefs.preferred_system_device,
                )
            }
            Err(e) => {
                warn!(
                    "Failed to load recording preferences, using defaults: {}",
                    e
                );
                (true, None, None)
            }
        };

    #[cfg(not(target_os = "macos"))]
    let microphone_device = resolve_mic_or_default(&app, preferred_mic_name.as_deref());

    let system_device = resolve_system_or_default(preferred_system_name.as_deref());

    #[cfg(target_os = "macos")]
    prepare_audio_for_recording(system_device.as_deref()).await?;

    #[cfg(target_os = "macos")]
    let microphone_device = resolve_mic_or_default(&app, preferred_mic_name.as_deref());

    // Async-first approach - no more blocking operations!
    info!("🚀 Starting async recording initialization");

    // Create new recording manager only after startup validation succeeds
    let mut manager = RecordingManager::new();
    let diarization_enabled = crate::diarization::commands::diarization_enabled_for_app(&app).await;
    manager.set_diarization_enabled(diarization_enabled);

    // Always ensure a meeting name is set so incremental saver initializes
    let effective_meeting_name = meeting_name.clone().unwrap_or_else(|| {
        // Example: Meeting 2025-10-03_08-25-23
        let now = chrono::Local::now();
        format!("Meeting {}", now.format("%Y-%m-%d_%H-%M-%S"))
    });
    manager.set_meeting_name(Some(effective_meeting_name));

    // Set up error callback
    let app_for_error = app.clone();
    manager.set_error_callback(move |error| {
        let _ = app_for_error.emit("recording-error", error.user_message());
    });

    // Start recording with resolved devices (replaces start_recording_with_defaults_and_auto_save call)
    let transcription_receiver = manager
        .start_recording(microphone_device, system_device, auto_save)
        .await
        .map_err(|error| map_recording_start_error(&app, error))?;

    // Take the device event receiver BEFORE storing manager globally.
    // A background task will process device events (hot-swap) without frontend polling.
    let device_event_receiver = manager.take_device_event_receiver();
    let session = manager.get_state().clone();

    // Store the manager globally to keep it alive
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        *global_manager = Some(manager);
    }

    // Spawn background device event processor (mic-disconnect fallback).
    if let Some(receiver) = device_event_receiver {
        spawn_device_event_processor(app.clone(), receiver, session.clone());
    }

    // Flip recording live + reset per-session flags (speech-detected latch,
    // mic-recovery budget). Shared with the other start path — see helper.
    finalize_recording_start();

    // CRITICAL: Listen for transcript-update events and save to recording manager
    // This enables transcript history persistence for page reload sync
    // Store listener ID for cleanup during stop_recording to ensure microphone is released
    {
        use tauri::Listener;
        let listener_id = app.listen("transcript-update", move |event: tauri::Event| {
            // Parse the transcript update from the event payload
            if let Ok(update) = serde_json::from_str::<TranscriptUpdate>(event.payload()) {
                // Create structured transcript segment
                let segment = update.into();

                // Save to recording manager
                if let Ok(manager_guard) = RECORDING_MANAGER.lock() {
                    if let Some(manager) = manager_guard.as_ref() {
                        manager.add_transcript_segment(segment);
                    }
                }
            }
        });
        let mut global_listener = TRANSCRIPT_LISTENER_ID.lock().unwrap();
        *global_listener = Some(listener_id);
        info!("✅ Transcript-update event listener registered for history persistence");
    }

    // Emit success event
    let start_event_result = app.emit(
        "recording-started",
        serde_json::json!({
            "message": "Recording started successfully with parallel processing",
            "devices": ["Default Microphone", "Default System Audio"],
            "workers": 3
        }),
    )
    .map_err(|e| e.to_string());

    // Update tray menu to reflect recording state
    crate::tray::update_tray_menu(&app);

    // Keep every resource under shutdown control before returning a start error.
    start_managed_transcription(app.clone(), transcription_receiver, session.clone());
    drop(engine_lifecycle_guard);
    complete_recording_start(start_event_result, || {
        stop_recording_session(app, Some(session), None)
    }).await?;

    info!("✅ Recording started successfully with async-first approach");

    Ok(())
}

/// Start recording with specific devices
pub async fn start_recording_with_devices<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
) -> Result<(), String> {
    start_recording_with_devices_and_meeting(app, mic_device_name, system_device_name, None).await
}

/// Start recording with specific devices and optional meeting name
pub async fn start_recording_with_devices_and_meeting<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
    meeting_name: Option<String>,
) -> Result<(), String> {
    info!(
        "Starting recording with specific devices: mic={:?}, system={:?}, meeting={:?}",
        mic_device_name, system_device_name, meeting_name
    );

    let engine_lifecycle_guard = super::common::acquire_engine_lifecycle_lock().await;

    // Check if already recording
    let current_recording_state = IS_RECORDING.load(Ordering::SeqCst);
    info!("🔍 IS_RECORDING state check: {}", current_recording_state);
    if current_recording_state {
        return Err("Recording already in progress".to_string());
    }

    if let Err(error) = crate::ensure_onnx_runtime_available() {
        return Err(map_recording_start_error(
            &app,
            RecordingStartError::TranscriptionRuntime(error),
        ));
    }

    // Validate that transcription models are available before starting recording
    info!("🔍 Validating transcription model availability before starting recording...");
    if let Err(validation_error) = transcription::validate_transcription_model_ready(&app).await {
        error!("Model validation failed: {}", validation_error);

        // Emit error event for frontend - actionable: false to show toast instead of modal
        // (download progress is already shown in top-right toast)
        let _ = app.emit("transcription-error", serde_json::json!({
            "error": validation_error,
            "userMessage": format!("Recording cannot start: {}", validation_error),
            "actionable": false,
            "phase": "startup"
        }));

        return Err(validation_error);
    }
    info!("✅ Transcription model validation passed");

    // Notify frontend that startup has begun (surfaces STARTING state)
    app.emit("recording-starting", serde_json::json!({
        "message": "Recording initialization started"
    })).map_err(|e| e.to_string())?;

    #[cfg(not(target_os = "macos"))]
    let mic_device = resolve_mic_or_default(&app, mic_device_name.as_deref());

    let system_device = resolve_system_or_default(system_device_name.as_deref());

    #[cfg(target_os = "macos")]
    prepare_audio_for_recording(system_device.as_deref()).await?;

    #[cfg(target_os = "macos")]
    let mic_device = resolve_mic_or_default(&app, mic_device_name.as_deref());

    // Async-first approach for custom devices - no more blocking operations!
    info!("🚀 Starting async recording initialization with custom devices");

    // Create new recording manager
    let mut manager = RecordingManager::new();
    let diarization_enabled = crate::diarization::commands::diarization_enabled_for_app(&app).await;
    manager.set_diarization_enabled(diarization_enabled);

    // Load recording preferences to check auto_save setting
    let auto_save = match super::recording_preferences::load_recording_preferences(&app).await {
        Ok(prefs) => {
            info!(
                "📋 Loaded recording preferences: auto_save={}",
                prefs.auto_save
            );
            prefs.auto_save
        }
        Err(e) => {
            warn!(
                "Failed to load recording preferences, defaulting to auto_save=true: {}",
                e
            );
            true // Default to saving if preferences can't be loaded
        }
    };

    // Always ensure a meeting name is set so incremental saver initializes
    let effective_meeting_name = meeting_name.clone().unwrap_or_else(|| {
        let now = chrono::Local::now();
        format!("Meeting {}", now.format("%Y-%m-%d_%H-%M-%S"))
    });
    manager.set_meeting_name(Some(effective_meeting_name));

    // Set up error callback
    let app_for_error = app.clone();
    manager.set_error_callback(move |error| {
        let _ = app_for_error.emit("recording-error", error.user_message());
    });

    // Start recording with specified devices and auto_save setting
    let transcription_receiver = manager
        .start_recording(mic_device, system_device, auto_save)
        .await
        .map_err(|error| map_recording_start_error(&app, error))?;

    // Take the device event receiver BEFORE storing manager globally.
    // A background task will process device events (hot-swap) without frontend polling.
    let device_event_receiver = manager.take_device_event_receiver();
    let session = manager.get_state().clone();

    // Store the manager globally to keep it alive
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        *global_manager = Some(manager);
    }

    // Spawn background device event processor (mic-disconnect fallback).
    if let Some(receiver) = device_event_receiver {
        spawn_device_event_processor(app.clone(), receiver, session.clone());
    }

    // Flip recording live + reset per-session flags (speech-detected latch,
    // mic-recovery budget). Shared with the other start path — see helper.
    finalize_recording_start();

    // CRITICAL: Listen for transcript-update events and save to recording manager
    // This enables transcript history persistence for page reload sync
    // Store listener ID for cleanup during stop_recording to ensure microphone is released
    {
        use tauri::Listener;
        let listener_id = app.listen("transcript-update", move |event: tauri::Event| {
            // Parse the transcript update from the event payload
            if let Ok(update) = serde_json::from_str::<TranscriptUpdate>(event.payload()) {
                // Create structured transcript segment
                let segment = update.into();

                // Save to recording manager
                if let Ok(manager_guard) = RECORDING_MANAGER.lock() {
                    if let Some(manager) = manager_guard.as_ref() {
                        manager.add_transcript_segment(segment);
                    }
                }
            }
        });
        let mut global_listener = TRANSCRIPT_LISTENER_ID.lock().unwrap();
        *global_listener = Some(listener_id);
        info!("✅ Transcript-update event listener registered for history persistence");
    }

    // Emit success event
    let start_event_result = app.emit(
        "recording-started",
        serde_json::json!({
            "message": "Recording started with custom devices and parallel processing",
            "devices": [
                mic_device_name.unwrap_or_else(|| "Default Microphone".to_string()),
                system_device_name.unwrap_or_else(|| "Default System Audio".to_string())
            ],
            "workers": 3
        }),
    )
    .map_err(|e| e.to_string());

    // Update tray menu to reflect recording state
    crate::tray::update_tray_menu(&app);

    // Keep every resource under shutdown control before returning a start error.
    start_managed_transcription(app.clone(), transcription_receiver, session.clone());
    drop(engine_lifecycle_guard);
    complete_recording_start(start_event_result, || {
        stop_recording_session(app, Some(session), None)
    }).await?;

    info!("✅ Recording started with custom devices using async-first approach");

    Ok(())
}

/// Keep worker failure handling outside the worker: shutdown joins the worker.
/// Store its handle before starting the failure monitor.
fn start_managed_transcription<R: Runtime>(
    app: AppHandle<R>,
    receiver: tokio::sync::mpsc::UnboundedReceiver<super::AudioChunk>,
    session: Arc<super::RecordingState>,
) {
    let worker = transcription::start_transcription_task(app.clone(), receiver);
    let (worker, completion) = supervise_transcription_task(worker);
    *TRANSCRIPTION_TASK.lock().unwrap() = Some(worker);
    tokio::spawn(async move {
        let result = completion.await.unwrap_or_else(|_| {
            Err("The speech recognition supervisor stopped unexpectedly".to_string())
        });
        if let Err(error) = result {
            if let Err(error) = stop_recording_session(app, Some(session), Some(error)).await {
                error!("Recording shutdown after a transcription failure failed: {error}");
            }
        }
    });
}

/// Roll back native capture before returning a failed startup notification.
async fn complete_recording_start<F, S>(
    result: Result<(), String>,
    stop: F,
) -> Result<(), String>
where
    F: FnOnce() -> S,
    S: std::future::Future<Output = Result<bool, String>>,
{
    if let Err(start_error) = result {
        return match stop().await {
            Ok(_) => Err(format!("Could not report recording start: {start_error}")),
            Err(stop_error) => Err(format!(
                "Could not report recording start: {start_error}. {stop_error}"
            )),
        };
    }
    Ok(())
}

/// Claim shutdown and its manager before sending any failure event.
fn claim_recording_stop<'a>(
    recording: &AtomicBool,
    stopping: &'a AtomicBool,
    manager: &Mutex<Option<RecordingManager>>,
    expected: Option<&Arc<super::RecordingState>>,
) -> Option<(StoppingGuard<'a>, Option<RecordingManager>)> {
    if !recording.load(Ordering::SeqCst) {
        return None;
    }
    let guard = StoppingGuard::try_acquire(stopping)?;
    let mut manager = manager.lock().unwrap();
    if !recording.load(Ordering::SeqCst)
        || !matches_stop_session(manager.as_ref().map(|manager| manager.get_state()), expected)
    {
        return None;
    }
    Some((guard, manager.take()))
}

/// Share the worker result with the failure monitor and the stop command.
/// The monitor must not own the handle that shutdown waits for.
fn supervise_transcription_task(
    worker: JoinHandle<Result<(), String>>,
) -> (
    JoinHandle<Result<(), String>>,
    tokio::sync::oneshot::Receiver<Result<(), String>>,
) {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let supervised = tokio::spawn(async move {
        let result = worker
            .await
            .unwrap_or_else(|error| Err(format!("The speech recognition worker failed: {error}")));
        let _ = sender.send(result.clone());
        result
    });
    (supervised, receiver)
}

/// A delayed worker failure can stop only the recording that owns the worker.
fn matches_stop_session(
    current: Option<&Arc<super::RecordingState>>,
    expected: Option<&Arc<super::RecordingState>>,
) -> bool {
    expected.map_or(true, |expected| {
        current.map_or(false, |current| Arc::ptr_eq(current, expected))
    })
}

/// Local engine to release after a recording stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelUnload {
    Whisper,
    Parakeet,
    FunAsr,
    None,
}

/// Map the saved transcript provider to the engine that was actually used.
///
/// Unknown providers deliberately map to `None`: the old catch-all unloaded
/// Whisper, which both leaked the real engine and could force an unnecessary
/// reload. `None` for the provider means the config could not be read and
/// keeps the previous Whisper fallback.
fn model_to_unload(provider: Option<&str>) -> ModelUnload {
    match provider {
        Some("parakeet") => ModelUnload::Parakeet,
        Some("funasr") => ModelUnload::FunAsr,
        Some("localWhisper") | None => ModelUnload::Whisper,
        Some(_) => ModelUnload::None,
    }
}

/// Return true when queued transcript chunks may not have been processed.
fn transcription_wait_lost_chunks(
    result: Result<Result<Result<(), String>, tokio::task::JoinError>, tokio::time::error::Elapsed>,
) -> bool {
    match result {
        Ok(Ok(Ok(()))) => {
            info!("✅ ALL transcription chunks processed successfully - no data lost");
            false
        }
        Ok(Ok(Err(error))) => {
            warn!("Transcription finished with missing chunks or a failed worker: {error}");
            true
        }
        Ok(Err(e)) => {
            // Continue anyway, but queued final chunks may not have been transcribed.
            warn!("⚠️ Transcription task completed with error: {:?}", e);
            true
        }
        Err(_) => {
            // Continue shutdown even on timeout - better to lose some chunks than hang forever
            warn!("⏱️ Transcription timeout (10 minutes) reached, continuing shutdown to prevent indefinite hang");
            true
        }
    }
}

/// Report audio failures as save errors and final transcript failures as
/// warnings. The manager has already logged both.
fn recording_stop_errors(outcome: RecordingStopOutcome) -> (Option<String>, bool) {
    let shutdown_error = outcome.audio_error.map(|error| {
        format!("Recording shutdown failed: {error:#}. Audio may be incomplete.")
    });
    (shutdown_error, outcome.transcript_error.is_some())
}

/// Stop and save one recording. Return false for an inactive recording or a
/// duplicate stop. Only the owning call may trigger frontend post-processing.
pub async fn stop_recording<R: Runtime>(
    app: AppHandle<R>,
    _args: RecordingArgs,
) -> Result<bool, String> {
    stop_recording_session(app, None, None).await
}

async fn stop_recording_session<R: Runtime>(
    app: AppHandle<R>,
    expected_session: Option<Arc<super::RecordingState>>,
    fatal_error: Option<String>,
) -> Result<bool, String> {
    info!(
        "🛑 Starting optimized recording shutdown - ensuring ALL transcript chunks are preserved"
    );

    // Wait for start to store the manager, worker, and listener together.
    let engine_lifecycle_guard = super::common::acquire_engine_lifecycle_lock().await;

    let Some((_stopping_guard, manager_for_cleanup)) = claim_recording_stop(
        &IS_RECORDING,
        &IS_RECORDING_STOPPING,
        &RECORDING_MANAGER,
        expected_session.as_ref(),
    ) else {
        return Ok(false);
    };

    // Only the owner may report this fatal error. A delayed failure must not
    // open the model selector after its recording stopped or a new one started.
    let save_failed_worker_recording = fatal_error.is_some();
    if let Some(error) = fatal_error {
        let _ = app.emit("transcription-error", serde_json::json!({
            "error": error,
            "userMessage": format!("Speech recognition failed: {error}. Recording is stopping. Check your model settings."),
            "actionable": true,
            "phase": "active"
        }));
    }
    // Model unload below can acquire this lock. Release it before waiting for
    // worker completion or doing any engine cleanup.
    drop(engine_lifecycle_guard);

    // Emit shutdown progress to frontend
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "stopping_audio",
            "message": "Stopping audio capture...",
            "progress": 20
        }),
    );

    // Step 1: Stop audio capture immediately (no more new chunks) with proper error handling
    let stop_result = if let Some(mut manager) = manager_for_cleanup {
        // Use FORCE FLUSH to immediately process all accumulated audio - eliminates 30s delay!
        info!("🚀 Using FORCE FLUSH to eliminate pipeline accumulation delays");
        let result = manager.stop_streams_and_force_flush().await;
        // Store manager back for later cleanup
        let manager_for_cleanup = Some(manager);
        (result, manager_for_cleanup)
    } else {
        warn!("No recording manager found to stop");
        (RecordingStopOutcome::default(), None)
    };

    let (stop_result, manager_for_cleanup) = stop_result;

    let (shutdown_error, mut transcript_incomplete) = recording_stop_errors(stop_result);

    // Step 1.5: Clean up transcript listener to release microphone
    // Unlisten transcript-update event to prevent lingering references
    {
        use tauri::Listener;
        if let Some(listener_id) = TRANSCRIPT_LISTENER_ID.lock().unwrap().take() {
            app.unlisten(listener_id);
            info!("✅ Transcript-update listener removed");
        }
    }

    // Step 2: Signal transcription workers to finish processing ALL queued chunks
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "processing_transcripts",
            "message": "Processing remaining transcript chunks...",
            "progress": 40
        }),
    );

    // Wait for transcription task with enhanced progress monitoring (NO TIMEOUT - we must process all chunks)
    let transcription_task = {
        let mut global_task = TRANSCRIPTION_TASK.lock().unwrap();
        global_task.take()
    };

    if let Some(task_handle) = transcription_task {
        info!("⏳ Waiting for ALL transcription chunks to be processed (no timeout - preserving every chunk)");

        // Enhanced progress monitoring during shutdown
        let progress_app = app.clone();
        let progress_task = tokio::spawn(async move {
            let last_update = std::time::Instant::now();

            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                // Emit periodic progress updates during shutdown
                let elapsed = last_update.elapsed().as_secs();
                let _ = progress_app.emit(
                    "recording-shutdown-progress",
                    serde_json::json!({
                        "stage": "processing_transcripts",
                        "message": format!("Processing transcripts... ({}s elapsed)", elapsed),
                        "progress": 40,
                        "detailed": true,
                        "elapsed_seconds": elapsed
                    }),
                );
            }
        });

        // Wait up to 10 minutes for transcription completion to prevent indefinite hangs
        let wait_result = tokio::time::timeout(
            tokio::time::Duration::from_secs(600), // 10 minutes max
            task_handle,
        )
        .await;
        if transcription_wait_lost_chunks(wait_result) {
            transcript_incomplete = true;
        }

        // Stop progress monitoring
        progress_task.abort();
    } else {
        info!("ℹ️ No transcription task found to wait for");
    }

    // Step 3: Now safely unload Whisper model after ALL chunks are processed
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "unloading_model",
            "message": "Unloading speech recognition model...",
            "progress": 70
        }),
    );

    info!("🧠 All transcript chunks processed. Now safely unloading transcription model...");

    // Determine which provider was used and unload the appropriate model (with timeout)
    let config = match tokio::time::timeout(
        tokio::time::Duration::from_secs(30), // 30 seconds max for DB operation
        crate::api::api::api_get_transcript_config(app.clone(), app.clone().state(), None),
    )
    .await
    {
        Ok(Ok(Some(config))) => Some(config.provider),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            warn!("⚠️ Failed to get transcript config: {:?}", e);
            None
        }
        Err(_) => {
            warn!("⏱️ Transcript config timeout (30s), continuing shutdown");
            None
        }
    };

    match model_to_unload(config.as_deref()) {
        ModelUnload::Parakeet => {
            info!("🦜 Unloading Parakeet model...");
            let engine_clone = {
                let engine_guard = crate::parakeet_engine::commands::PARAKEET_ENGINE
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                engine_guard.as_ref().cloned()
            };

            if let Some(engine) = engine_clone {
                let current_model = engine
                    .get_current_model()
                    .await
                    .unwrap_or_else(|| "unknown".to_string());
                info!("Current Parakeet model before unload: '{}'", current_model);

                if engine.unload_model().await {
                    info!(
                        "✅ Parakeet model '{}' unloaded successfully",
                        current_model
                    );
                } else {
                    warn!("⚠️ Failed to unload Parakeet model '{}'", current_model);
                }
            } else {
                warn!("⚠️ No Parakeet engine found to unload model");
            }
        }
        ModelUnload::FunAsr => {
            info!("Unloading FUN-ASR model...");
            // The helper owns the ~1.45 GB encoder/decoder/KV cache; leave
            // Whisper alone or FUN-ASR stays resident until app exit.
            //
            // A transcription can hold the sidecar for up to its 120 s
            // request timeout. Bound the unload so a shutdown that already
            // timed out waiting for chunks does not block for two more
            // minutes; the helper exits with the app if it is still busy.
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                crate::funasr_engine::commands::funasr_unload_model(),
            )
            .await
            {
                Ok(Ok(true)) => info!("✅ FUN-ASR model unloaded successfully"),
                Ok(Ok(false)) => info!("ℹ️ No FUN-ASR model was loaded"),
                Ok(Err(e)) => warn!("⚠️ Failed to unload FUN-ASR model: {}", e),
                Err(_) => warn!(
                    "⏱️ FUN-ASR unload timed out; the helper will exit when the app closes"
                ),
            }
        }
        ModelUnload::Whisper => {
            info!("🎤 Unloading Whisper model...");
            let engine_clone = {
                let engine_guard = crate::whisper_engine::commands::WHISPER_ENGINE
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                engine_guard.as_ref().cloned()
            };

            if let Some(engine) = engine_clone {
                let current_model = engine
                    .get_current_model()
                    .await
                    .unwrap_or_else(|| "unknown".to_string());
                info!("Current Whisper model before unload: '{}'", current_model);

                if engine.unload_model().await {
                    info!("✅ Whisper model '{}' unloaded successfully", current_model);
                } else {
                    warn!("⚠️ Failed to unload Whisper model '{}'", current_model);
                }
            } else {
                warn!("⚠️ No Whisper engine found to unload model");
            }
        }
        ModelUnload::None => {
            warn!("⚠️ Unrecognized transcription provider; leaving local models loaded");
        }
    }

    // Step 4: Finalize recording state and cleanup resources safely
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "finalizing",
            "message": "Finalizing recording and cleaning up resources...",
            "progress": 90
        }),
    );

    // Perform final cleanup with the manager if available
    let (meeting_folder, meeting_name, save_error) = if let Some(mut manager) = manager_for_cleanup {
        info!("🧹 Performing final cleanup and saving recording data");

        // Extract meeting info BEFORE async operations
        let meeting_folder = manager.get_meeting_folder();
        let meeting_name = manager.get_meeting_name();

        let save_error = match tokio::time::timeout(
            tokio::time::Duration::from_secs(300),
            manager.save_recording_only(&app),
        ).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("{error:#}")),
            Err(_) => Some("Recording save timed out after 5 minutes".to_string()),
        };
        let save_error = save_error.map(|error| {
            let location = meeting_folder.as_ref()
                .map(|folder| format!(" Files kept in: {}.", folder.display()))
                .unwrap_or_default();
            format!("Recording save failed: {error}.{location} Audio may be incomplete.")
        });
        (meeting_folder, meeting_name, save_error)
    } else {
        (None, None, Some("Recording save failed: recording manager is missing".to_string()))
    };

    let save_error = match (shutdown_error, save_error) {
        (Some(shutdown), Some(save)) => Some(format!("{shutdown} {save}")),
        (shutdown, save) => shutdown.or(save),
    };

    // Set recording flag to false
    info!("🔍 Setting IS_RECORDING to false");
    IS_RECORDING.store(false, Ordering::SeqCst);
    // IS_RECORDING_STOPPING is cleared by _stopping_guard on scope exit.

    // Step 4.5: Prepare metadata for frontend (NO database save)
    // NOTE: We do NOT save to database here. The frontend will save after all transcripts are displayed.
    // This ensures the user sees all transcripts streaming in before the database save happens.
    let (folder_path_str, meeting_name_str) = match (&meeting_folder, &meeting_name) {
        (Some(path), Some(name)) => (Some(path.to_string_lossy().to_string()), Some(name.clone())),
        _ => (None, None),
    };

    info!("📤 Preparing recording metadata for frontend save");
    info!("   folder_path: {:?}", folder_path_str);
    info!("   meeting_name: {:?}", meeting_name_str);

    // Database save removed - frontend will handle this after receiving all transcripts
    info!("ℹ️ Skipping database save in Rust - frontend will save after all transcripts received");

    let transcript_warning = transcript_incomplete.then(|| {
        let location = meeting_folder.as_ref()
            .map(|folder| format!(" Recording folder: {}.", folder.display()))
            .unwrap_or_default();
        format!(
            "The final transcript may be incomplete. If saved audio is available, use Enhance in the meeting to transcribe it again.{location}"
        )
    });

    // Step 5: Complete shutdown
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": if save_error.is_some() { "error" } else if transcript_warning.is_some() { "warning" } else { "complete" },
            "message": save_error.as_deref().or(transcript_warning.as_deref())
                .unwrap_or("Recording stopped successfully"),
            "progress": 100
        }),
    );

    // Emit final stop event with folder_path and meeting_name for frontend to save
    app.emit(
        "recording-stopped",
        serde_json::json!({
            "message": save_error.as_deref().or(transcript_warning.as_deref())
                .unwrap_or("Recording stopped"),
            "folder_path": folder_path_str,
            "meeting_name": meeting_name_str,
            "save_error": save_error,
            "transcript_warning": transcript_warning
        }),
    )
    .map_err(|e| e.to_string())?;

    // Update tray menu to reflect stopped state
    crate::tray::update_tray_menu(&app);

    if let Some(error) = save_error {
        error!("{error}");
        return Err(error);
    }
    if expected_session.is_some() {
        // A failed worker still needs database persistence; transcript quality is
        // reported separately. Startup notification rollback only resets the UI.
        app.emit("recording-stop-complete", save_failed_worker_recording)
            .map_err(|error| error.to_string())?;
    }
    if transcript_warning.is_some() {
        warn!("Recording stopped and saved; the final transcript may be incomplete");
    } else {
        info!("Recording stopped and saved successfully");
    }
    Ok(true)
}

/// Check if recording is active
pub async fn is_recording() -> bool {
    IS_RECORDING.load(Ordering::SeqCst)
}

/// Get recording statistics
pub async fn get_transcription_status() -> TranscriptionStatus {
    TranscriptionStatus {
        chunks_in_queue: 0,
        is_processing: IS_RECORDING.load(Ordering::SeqCst),
        last_activity_ms: 0,
    }
}

/// Pause the current recording
#[tauri::command]
pub async fn pause_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    info!("Pausing recording");

    // Check if currently recording
    if !IS_RECORDING.load(Ordering::SeqCst) {
        return Err("No recording is currently active".to_string());
    }

    // Access the recording manager and pause it
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.pause_recording().map_err(|e| e.to_string())?;

        // Emit pause event to frontend
        app.emit(
            "recording-paused",
            serde_json::json!({
                "message": "Recording paused"
            }),
        )
        .map_err(|e| e.to_string())?;

        // Update tray menu to reflect paused state
        crate::tray::update_tray_menu(&app);

        info!("Recording paused successfully");
        Ok(())
    } else {
        Err("No recording manager found".to_string())
    }
}

/// Resume the current recording
#[tauri::command]
pub async fn resume_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    info!("Resuming recording");

    // Check if currently recording
    if !IS_RECORDING.load(Ordering::SeqCst) {
        return Err("No recording is currently active".to_string());
    }

    // Access the recording manager and resume it
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.resume_recording().map_err(|e| e.to_string())?;

        // Emit resume event to frontend
        app.emit(
            "recording-resumed",
            serde_json::json!({
                "message": "Recording resumed"
            }),
        )
        .map_err(|e| e.to_string())?;

        // Update tray menu to reflect resumed state
        crate::tray::update_tray_menu(&app);

        info!("Recording resumed successfully");
        Ok(())
    } else {
        Err("No recording manager found".to_string())
    }
}

/// Check if recording is currently paused
#[tauri::command]
pub async fn is_recording_paused() -> bool {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.is_paused()
    } else {
        false
    }
}

/// Get detailed recording state
#[tauri::command]
pub async fn get_recording_state() -> serde_json::Value {
    let is_recording = IS_RECORDING.load(Ordering::SeqCst);
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        serde_json::json!({
            "is_recording": is_recording,
            "is_paused": manager.is_paused(),
            "is_active": manager.is_active(),
            "recording_duration": manager.get_recording_duration(),
            "active_duration": manager.get_active_recording_duration(),
            "total_pause_duration": manager.get_total_pause_duration(),
            "current_pause_duration": manager.get_current_pause_duration()
        })
    } else {
        serde_json::json!({
            "is_recording": is_recording,
            "is_paused": false,
            "is_active": false,
            "recording_duration": null,
            "active_duration": null,
            "total_pause_duration": 0.0,
            "current_pause_duration": null
        })
    }
}

/// Get the meeting folder path for the current recording
/// Returns the path if a meeting name was set and folder structure initialized
#[tauri::command]
pub async fn get_meeting_folder_path() -> Result<Option<String>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager
            .get_meeting_folder()
            .map(|p| p.to_string_lossy().to_string()))
    } else {
        Ok(None)
    }
}

/// Get accumulated transcript segments from current recording session
/// Used for syncing frontend state after page reload during active recording
#[tauri::command]
pub async fn get_transcript_history(
) -> Result<Vec<crate::audio::recording_saver::TranscriptSegment>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager.get_transcript_segments())
    } else {
        Ok(Vec::new()) // No recording active, return empty
    }
}

/// Get meeting name from current recording session
/// Used for syncing frontend state after page reload during active recording
#[tauri::command]
pub async fn get_recording_meeting_name() -> Result<Option<String>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager.get_meeting_name())
    } else {
        Ok(None)
    }
}

// ============================================================================
// DEVICE MONITORING COMMANDS (AirPods/Bluetooth disconnect/reconnect support)
// ============================================================================

/// Get information about the active audio output device
/// Used to warn users about Bluetooth playback issues
#[tauri::command]
pub async fn get_active_audio_output() -> Result<super::playback_monitor::AudioOutputInfo, String> {
    super::playback_monitor::get_active_audio_output()
        .await
        .map_err(|e| format!("Failed to get audio output info: {}", e))
}


// ============================================================================
// MIC HOT-SWAP (disconnect recovery)
// ============================================================================

// Guard against concurrent mic hot-swap tasks. Only used by the disconnect
// fallback path (trigger_mic_fallback_to_default) — the "chase the new
// default" auto-swap has been removed.
static MIC_SWAP_IN_PROGRESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// Bounded retry budget for the disconnect fallback (P1 #2). Counts COMPLETED
// failed attempts; MIC_SWAP_IN_PROGRESS still prevents overlapping swaps.
static MIC_FALLBACK_FAILED_ATTEMPTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const MAX_MIC_FALLBACK_ATTEMPTS: u32 = 3;

/// Perform mic hot-swap using phased locking — never holds RECORDING_MANAGER during I/O
/// except the brief mic-stream stop in Phase 1.
/// If CPAL hangs during stream creation, only this task blocks; stop flow stays unblocked.
async fn perform_mic_hot_swap_task<R: Runtime>(
    new_device_name: String,
    session: &Arc<super::RecordingState>,
    app: AppHandle<R>,
) -> Result<(), String> {
    info!("[HOT_SWAP] Starting mic hot-swap to '{}'", new_device_name);

    match do_mic_swap(&new_device_name, session).await {
        Ok(()) => {
            info!("[HOT_SWAP] Mic switched to '{}'", new_device_name);
            let _ = app.emit("mic-device-switched", serde_json::json!({
                "device_name": new_device_name
            }));
            Ok(())
        }
        Err(e) => {
            if !session_live(session) {
                return Err(e);
            }
            warn!("[HOT_SWAP] First attempt failed: {} — retrying in 500ms", e);
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            match do_mic_swap(&new_device_name, session).await {
                Ok(()) => {
                    info!("[HOT_SWAP] Mic switched to '{}' on retry", new_device_name);
                    let _ = app.emit("mic-device-switched", serde_json::json!({
                        "device_name": new_device_name
                    }));
                    Ok(())
                }
                Err(e) => {
                    error!("[HOT_SWAP] Mic swap failed after retry: {}", e);
                    if session_live(session) {
                        let _ = app.emit("mic-swap-failed", serde_json::json!({
                            "error": e,
                            "device_name": new_device_name
                        }));
                    }
                    Err(e)
                }
            }
        }
    }
}

/// Phased mic swap — lock is never held during async I/O.
async fn do_mic_swap(device_name: &str, session: &Arc<super::RecordingState>) -> Result<(), String> {
    // Phase 1: Lock briefly — verify identity, take old stream OUT (no teardown under lock)
    let old_mic = {
        let mut guard = RECORDING_MANAGER.lock().unwrap();
        let manager = guard.as_mut().ok_or_else(|| "Recording manager not available".to_string())?;
        if !manager.is_recording() {
            return Err("Recording stopped — aborting mic hot-swap".to_string());
        }
        if !Arc::ptr_eq(manager.get_state(), session) {
            return Err("Session changed before hot-swap — aborting".to_string());
        }
        manager.take_mic_stream_for_swap()
    }; // lock released

    // Tear down the dead mic OUTSIDE the lock — cpal stop()/drop on a
    // disconnected BT device can stall on the CoreAudio HAL lock; doing it
    // under RECORDING_MANAGER would freeze stop_recording (deep-review #2).
    // Non-fatal: the replacement stream is created next regardless, so a
    // teardown error/stall on the already-dead device must not abort the swap.
    if let Some(s) = old_mic {
        if let Err(e) = s.stop() {
            warn!("[HOT_SWAP] Failed to stop old mic stream (proceeding): {}", e);
        }
    }

    // Phase 2: Async I/O WITHOUT lock — may be slow, that's OK
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Build the AudioDevice directly from the name — the caller
    // (trigger_mic_fallback_to_default) already resolved it via
    // default_input_device(). Skipping list_audio_devices() here avoids a
    // full cpal enumeration on the exact BT-transition hot path where it's
    // known to hang 100+ s (see H2 in PR-175 review). The real device
    // validation happens inside AudioStream::create → get_device_and_config
    // which does a targeted host.input_devices() lookup by name.
    let device_arc = std::sync::Arc::new(super::AudioDevice::new(
        device_name.to_string(),
        super::DeviceType::Input,
    ));

    info!("[HOT_SWAP] Creating new mic stream for '{}' (lock released)", device_name);
    let new_stream = super::stream::AudioStream::create(
        device_arc.clone(),
        session.clone(),
        super::recording_state::DeviceType::Microphone,
        None,
    ).await.map_err(|e| format!("Failed to create mic stream: {}", e))?;

    // Resolve the current default output OUTSIDE the lock — a CoreAudio stall here
    // must not block stop_recording (which needs RECORDING_MANAGER). (P1 #1)
    let system_name = default_output_device().ok().map(|d| d.name);

    // Phase 3: Lock briefly — install ONLY if still the same session
    {
        let mut guard = RECORDING_MANAGER.lock().unwrap();
        match guard.as_mut() {
            Some(manager) if Arc::ptr_eq(manager.get_state(), session) => {
                manager.set_mic_stream_after_swap(new_stream, device_arc, system_name);
                info!("[HOT_SWAP] Mic hot-swap to '{}' completed", device_name);
            }
            Some(_) => {
                return Err("Session changed during hot-swap — discarding stale mic stream".to_string());
            }
            None => {
                return Err("Recording manager gone during hot-swap".to_string());
            }
        }
    } // lock released

    Ok(())
}

/// Background processor for device monitor events during a recording session.
///
/// The ONLY mid-recording mic switch that is allowed is the fallback from a
/// dead device to the system default, triggered by the device monitor's
/// DeviceDisconnected event. Any other device event is explicitly ignored —
/// recording stays on whatever device was picked at start time until the
/// meeting ends.
///
/// Rationale: auto-swapping to a freshly-connected BT device during recording
/// triggers a reliable hang inside cpal's stream creation on macOS. Locking
/// the device at start eliminates that hang and also makes the recording
/// session predictable.
///
/// The task stops automatically when the receiver is dropped (recording
/// ends / monitor stops).
fn spawn_device_event_processor<R: Runtime>(
    app: AppHandle<R>,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<DeviceEvent>,
    session: Arc<super::RecordingState>,
) {
    tokio::spawn(async move {
        info!("[DEVICE_EVENTS] Background event processor started");

        while let Some(event) = receiver.recv().await {
            // Skip if recording has stopped
            if !recording_live() {
                info!("[DEVICE_EVENTS] Recording stopped — ignoring event: {:?}", event);
                continue;
            }

            match event {
                DeviceEvent::DeviceDisconnected { ref device_name, ref device_type } => {
                    info!("[DEVICE_EVENTS] Device disconnected: '{}' ({:?})", device_name, device_type);
                    // The only automatic mid-recording mic change allowed:
                    // when the active microphone dies, fall back to the
                    // system default input. Triggered after the device
                    // monitor's polling threshold fires.
                    if matches!(device_type, DeviceMonitorType::Microphone) {
                        let name = device_name.clone();
                        let app_clone = app.clone();
                        let session = session.clone();
                        tokio::spawn(async move {
                            trigger_mic_fallback_to_default(app_clone, name, session).await;
                        });
                    }
                }
                DeviceEvent::DeviceReconnected { ref device_name, ref device_type } => {
                    // Per product decision: once we have fallen back to the
                    // built-in mic we stay there for the rest of the meeting.
                    // This is intentional — just log and do nothing.
                    info!("[DEVICE_EVENTS] Device reconnected: '{}' ({:?}) — staying on current mic (fallback is sticky)", device_name, device_type);
                }
                DeviceEvent::DeviceListChanged => {
                    debug!("[DEVICE_EVENTS] Device list changed");
                }
            }
        }
        info!("[DEVICE_EVENTS] Background event processor stopped (channel closed)");
    });
}

/// Disconnect fallback: swap the active mic to the system default input
/// device. Triggered from the background device event processor after the
/// device monitor's polling threshold (3 × 2s) fires `DeviceDisconnected`
/// for the active microphone.
///
/// `disconnected_name` is the device that just died. We keep it to detect
/// the edge case where macOS hasn't yet updated the system default input
/// away from the dead device — we wait and retry in that case rather than
/// swapping back to the same broken device.
///
/// This function takes the MIC_SWAP_IN_PROGRESS guard itself; the caller
/// must NOT already hold it. If a swap is somehow already running this
/// returns immediately.
async fn trigger_mic_fallback_to_default<R: Runtime>(
    app: AppHandle<R>,
    disconnected_name: String,
    session: Arc<super::RecordingState>,
) {
    if !session_live(&session) {
        info!(
            "[MIC_FALLBACK] Not recording — skipping fallback for '{}'",
            disconnected_name
        );
        return;
    }

    if MIC_FALLBACK_FAILED_ATTEMPTS.load(Ordering::SeqCst) >= MAX_MIC_FALLBACK_ATTEMPTS {
        warn!(
            "[MIC_FALLBACK] {} failed attempts reached — giving up on '{}' (terminal event already announced)",
            MAX_MIC_FALLBACK_ATTEMPTS, disconnected_name
        );
        return;
    }

    if MIC_SWAP_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        info!(
            "[MIC_FALLBACK] Swap already in progress — skipping fallback for '{}'",
            disconnected_name
        );
        return;
    }

    // Guard that clears MIC_SWAP_IN_PROGRESS on any return path below so a
    // panic or early return can't leave the flag stuck.
    struct SwapGuard;
    impl Drop for SwapGuard {
        fn drop(&mut self) {
            MIC_SWAP_IN_PROGRESS.store(false, Ordering::SeqCst);
        }
    }
    let _guard = SwapGuard;

    info!(
        "[MIC_FALLBACK] Starting fallback from disconnected device '{}'",
        disconnected_name
    );

    // Let macOS finish swapping the system default input away from the dead
    // device. 150ms is enough in practice for the built-in mic to become the
    // default when an explicitly-selected BT device disconnects.
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

    // A Stop-A/Start-B during the sleep swapped our session out. Bail silently —
    // emitting or spending the recovery budget here would fire against B with
    // A's device. Covers the default_input_device() error branch below.
    if !session_live(&session) {
        info!("[MIC_FALLBACK] Session no longer live after wait — aborting fallback for '{}'", disconnected_name);
        return;
    }

    // Query the current system default input. If it still reports the
    // disconnected device, back off once more and re-query — this handles
    // the edge case where the OS hasn't propagated the change yet.
    let fallback_name = match default_input_device() {
        Ok(dev) => dev.name,
        Err(e) => {
            error!("[MIC_FALLBACK] Failed to query default input device: {}", e);
            let _ = app.emit(
                "mic-swap-failed",
                serde_json::json!({
                    "error": format!("Failed to query default input: {}", e),
                    "device_name": disconnected_name,
                }),
            );
            let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
            if n == MAX_MIC_FALLBACK_ATTEMPTS {
                let _ = app.emit(
                    "mic-recovery-exhausted",
                    serde_json::json!({ "device_name": disconnected_name }),
                );
            }
            return;
        }
    };

    let fallback_name = if fallback_name == disconnected_name {
        warn!(
            "[MIC_FALLBACK] Default input still reports disconnected device '{}' — retrying after 300ms",
            disconnected_name
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        if !session_live(&session) {
            info!("[MIC_FALLBACK] Session no longer live after retry wait — aborting fallback for '{}'", disconnected_name);
            return;
        }
        match default_input_device() {
            Ok(dev) if dev.name != disconnected_name => dev.name,
            Ok(dev) => {
                error!(
                    "[MIC_FALLBACK] Default input still '{}' after retry — aborting fallback",
                    dev.name
                );
                let _ = app.emit(
                    "mic-swap-failed",
                    serde_json::json!({
                        "error": "System default input still reports disconnected device after retry",
                        "device_name": disconnected_name,
                    }),
                );
                let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
                if n == MAX_MIC_FALLBACK_ATTEMPTS {
                    let _ = app.emit(
                        "mic-recovery-exhausted",
                        serde_json::json!({ "device_name": disconnected_name }),
                    );
                }
                return;
            }
            Err(e) => {
                error!("[MIC_FALLBACK] Failed to re-query default input device: {}", e);
                let _ = app.emit(
                    "mic-swap-failed",
                    serde_json::json!({
                        "error": format!("Failed to re-query default input: {}", e),
                        "device_name": disconnected_name,
                    }),
                );
                let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
                if n == MAX_MIC_FALLBACK_ATTEMPTS {
                    let _ = app.emit(
                        "mic-recovery-exhausted",
                        serde_json::json!({ "device_name": disconnected_name }),
                    );
                }
                return;
            }
        }
    } else {
        fallback_name
    };

    info!(
        "[MIC_FALLBACK] Falling back '{}' → '{}'",
        disconnected_name, fallback_name
    );

    // macOS Core Audio pre-wake for the hot-swap path — before we call the
    // rebuild path (which internally calls `AudioDeviceStart` on the new
    // mic), play 150ms of digital silence through the current system
    // output device to force the Core Audio hardware unit out of its idle
    // power state. Without this, `AudioDeviceStart` can return `noErr` but
    // the IO proc will not fire for 10-30 seconds until some other audio
    // nudges the hardware awake — the "backend idle until you play YouTube"
    // symptom from earlier testing.
    //
    // `wake_audio_connection_for_swap` has a built-in fallback: if the
    // current system device name doesn't enumerate (e.g. the BT output just
    // disappeared), it plays through `default_output_device()` instead,
    // which on macOS will now be the built-in speakers — exactly the
    // hardware unit we want to wake for the fallback mic.
    //
    // Non-fatal: on error we log and proceed to the swap anyway. A failed
    // wake is strictly better than no wake.
    #[cfg(target_os = "macos")]
    {
        // Read from the captured session directly (no manager lock) — this
        // stays correct even if the global manager has since been swapped by
        // a Stop/Start of a different session.
        let sys_device_name = session.get_system_device().map(|d| d.name.clone());
        if let Some(name) = sys_device_name {
            match super::recording_manager::wake_audio_connection_for_swap(&name).await {
                Ok(()) => info!("[MIC_FALLBACK] Pre-swap audio wake completed"),
                Err(e) => warn!(
                    "[MIC_FALLBACK] Pre-swap audio wake failed: {} — proceeding anyway",
                    e
                ),
            }
        } else {
            log::debug!("[MIC_FALLBACK] No system device recorded — skipping pre-swap wake");
        }
    }

    // Stop may have started during the sleeps above — bail before touching
    // the (possibly already taken) manager.
    if !session_live(&session) {
        info!("[MIC_FALLBACK] Recording stopping — aborting fallback for '{}'", disconnected_name);
        return;
    }

    // perform_mic_hot_swap_task performs its own retry-once logic on failure
    // and emits the mic-device-switched / mic-swap-failed events, so we can
    // just delegate here. It does NOT touch MIC_SWAP_IN_PROGRESS internally.
    match perform_mic_hot_swap_task(fallback_name.clone(), &session, app.clone()).await {
        Ok(()) => {
            info!(
                "[MIC_FALLBACK] Fallback complete: now recording via '{}'",
                fallback_name
            );
            MIC_FALLBACK_FAILED_ATTEMPTS.store(0, Ordering::SeqCst);
        }
        Err(e) => {
            error!("[MIC_FALLBACK] Fallback swap failed: {}", e);
            if !session_live(&session) {
                return;
            }
            let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
            if n == MAX_MIC_FALLBACK_ATTEMPTS {
                let _ = app.emit(
                    "mic-recovery-exhausted",
                    serde_json::json!({ "device_name": disconnected_name }),
                );
            }
        }
    }
}

#[cfg(test)]
mod unload_tests {
    use super::{model_to_unload, ModelUnload};

    #[test]
    fn funasr_stop_unloads_the_funasr_engine() {
        assert_eq!(model_to_unload(Some("funasr")), ModelUnload::FunAsr);
    }

    #[test]
    fn each_local_provider_maps_to_its_own_engine() {
        assert_eq!(model_to_unload(Some("localWhisper")), ModelUnload::Whisper);
        assert_eq!(model_to_unload(Some("parakeet")), ModelUnload::Parakeet);
    }

    #[test]
    fn unknown_providers_do_not_fall_back_to_whisper() {
        assert_eq!(model_to_unload(Some("deepgram")), ModelUnload::None);
        assert_eq!(model_to_unload(Some("bogus")), ModelUnload::None);
    }
}

#[cfg(test)]
mod stopping_tests {
    use super::{
        claim_recording_stop, complete_recording_start, matches_stop_session,
        supervise_transcription_task, RecordingManager, StoppingGuard,
    };
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Barrier, Mutex,
    };

    #[tokio::test]
    async fn successful_start_notification_keeps_capture_running() {
        assert!(complete_recording_start(Ok(()), || async {
            panic!("A successful start must not stop capture");
        }).await.is_ok());
    }

    #[tokio::test]
    async fn failed_start_notification_waits_for_capture_to_stop() {
        let recording = AtomicBool::new(true);
        let error = complete_recording_start(Err("Event send failed".to_string()), || async {
            tokio::task::yield_now().await;
            recording.store(false, Ordering::SeqCst);
            Ok(true)
        }).await.unwrap_err();
        assert!(!recording.load(Ordering::SeqCst));
        assert!(error.contains("Event send failed"));
    }

    #[tokio::test]
    async fn failed_start_keeps_both_notification_and_save_errors() {
        let error = complete_recording_start(Err("Event send failed".to_string()), || async {
            Err("Recording save failed".to_string())
        }).await.unwrap_err();
        assert!(error.contains("Event send failed"));
        assert!(error.contains("Recording save failed"));
    }

    #[test]
    fn stopped_worker_cannot_claim_shutdown_or_report_a_fatal_error() {
        let recording = AtomicBool::new(false);
        let stopping = AtomicBool::new(false);
        let manager = Mutex::new(None);
        let session = crate::audio::RecordingState::new();
        assert!(claim_recording_stop(&recording, &stopping, &manager, Some(&session)).is_none());
        assert!(!stopping.load(Ordering::SeqCst));
    }

    #[test]
    fn stale_worker_cannot_take_a_later_session_or_report_a_fatal_error() {
        let recording = AtomicBool::new(true);
        let stopping = AtomicBool::new(false);
        let manager = Mutex::new(Some(RecordingManager::new()));
        let current = manager.lock().unwrap().as_ref().unwrap().get_state().clone();
        let old = crate::audio::RecordingState::new();
        assert!(claim_recording_stop(&recording, &stopping, &manager, Some(&old)).is_none());
        assert!(matches_stop_session(
            manager.lock().unwrap().as_ref().map(|manager| manager.get_state()),
            Some(&current),
        ));
        assert!(!stopping.load(Ordering::SeqCst));
    }

    #[test]
    fn current_worker_claims_its_manager_once_before_reporting_an_error() {
        let recording = AtomicBool::new(true);
        let stopping = AtomicBool::new(false);
        let manager = Mutex::new(Some(RecordingManager::new()));
        let current = manager.lock().unwrap().as_ref().unwrap().get_state().clone();
        let owner = claim_recording_stop(&recording, &stopping, &manager, Some(&current));
        assert!(owner.as_ref().unwrap().1.is_some());
        assert!(manager.lock().unwrap().is_none());
        assert!(claim_recording_stop(&recording, &stopping, &manager, Some(&current)).is_none());
        drop(owner);
        assert!(!stopping.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn completed_worker_reports_success_to_monitor_and_stop() {
        let (task, completion) = supervise_transcription_task(tokio::spawn(async { Ok(()) }));
        assert_eq!(completion.await.unwrap(), Ok(()));
        assert_eq!(task.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn failed_worker_init_preserves_the_error_for_shutdown() {
        let (task, completion) =
            supervise_transcription_task(tokio::spawn(async { Err("Model load failed".to_string()) }));
        assert_eq!(
            completion.await.unwrap(),
            Err("Model load failed".to_string())
        );
        assert_eq!(task.await.unwrap(), Err("Model load failed".to_string()));
    }

    #[tokio::test]
    async fn panicked_worker_init_requests_shutdown() {
        let (task, completion) = supervise_transcription_task(tokio::spawn(async {
            panic!("Model initialization panicked");
        }));
        let error = completion.await.unwrap().unwrap_err();
        assert!(error.contains("Model initialization panicked"));
        assert_eq!(task.await.unwrap(), Err(error));
    }

    #[tokio::test]
    async fn worker_failure_after_init_reaches_monitor_before_stop() {
        let (ready_sender, ready) = tokio::sync::oneshot::channel();
        let (fail, failure) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            ready_sender.send(()).unwrap();
            failure.await.unwrap();
            panic!("Injected active worker panic");
        });
        let (task, mut completion) = supervise_transcription_task(worker);
        ready.await.unwrap();
        assert!(matches!(
            completion.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        fail.send(()).unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), completion)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("Injected active worker panic"));
        // Shutdown can join the stored handle after the monitor reports failure.
        assert_eq!(task.await.unwrap(), Err(error));
    }

    #[test]
    fn failed_worker_can_stop_only_its_own_session() {
        let failed_session = crate::audio::RecordingState::new();
        let later_session = crate::audio::RecordingState::new();
        assert!(matches_stop_session(Some(&failed_session), Some(&failed_session)));
        assert!(!matches_stop_session(Some(&later_session), Some(&failed_session)));
        assert!(!matches_stop_session(None, Some(&failed_session)));
        assert!(matches_stop_session(Some(&later_session), None));
    }

    #[test]
    fn concurrent_stops_have_only_one_owner() {
        let flag = AtomicBool::new(false);
        let owners = AtomicUsize::new(0);
        let start = Barrier::new(8);
        let finish = Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    start.wait();
                    let guard = StoppingGuard::try_acquire(&flag);
                    if guard.is_some() {
                        owners.fetch_add(1, Ordering::SeqCst);
                    }
                    finish.wait();
                    drop(guard);
                });
            }
        });
        assert_eq!(owners.load(Ordering::SeqCst), 1);
        assert!(!flag.load(Ordering::SeqCst));
        assert!(StoppingGuard::try_acquire(&flag).is_some());
    }

    #[test]
    fn unwind_releases_stop_ownership() {
        let flag = AtomicBool::new(false);
        let result = std::panic::catch_unwind(|| {
            let _guard = StoppingGuard::try_acquire(&flag).unwrap();
            panic!("stop failed");
        });
        assert!(result.is_err());
        assert!(StoppingGuard::try_acquire(&flag).is_some());
    }
}

#[cfg(test)]
mod stop_outcome_tests {
    use super::{recording_stop_errors, transcription_wait_lost_chunks, RecordingStopOutcome};

    #[tokio::test]
    async fn failed_or_timed_out_transcription_worker_marks_transcript_incomplete() {
        let finished = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::spawn(async { Ok(()) }),
        )
        .await;
        assert!(!transcription_wait_lost_chunks(finished));

        let panicked = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::spawn(async { panic!("Injected worker panic") }),
        )
        .await;
        assert!(transcription_wait_lost_chunks(panicked));

        let timed_out = tokio::time::timeout(
            std::time::Duration::from_millis(1),
            tokio::spawn(std::future::pending::<Result<(), String>>()),
        )
        .await;
        assert!(transcription_wait_lost_chunks(timed_out));
    }

    #[tokio::test]
    async fn reported_inner_worker_failure_marks_transcript_incomplete() {
        let failed = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::spawn(async { Err("Inner worker failed".to_string()) }),
        ).await;
        assert!(transcription_wait_lost_chunks(failed));
    }

    #[test]
    fn final_transcript_failure_does_not_become_an_audio_save_failure() {
        let (shutdown_error, transcript_incomplete) = recording_stop_errors(RecordingStopOutcome {
            audio_error: None,
            transcript_error: Some(anyhow::anyhow!("Final VAD flush failed")),
        });
        assert!(shutdown_error.is_none());
        assert!(transcript_incomplete);
    }

    #[test]
    fn audio_and_transcript_failures_are_both_preserved() {
        let (shutdown_error, transcript_incomplete) = recording_stop_errors(RecordingStopOutcome {
            audio_error: Some(anyhow::anyhow!("Capture could not stop")),
            transcript_error: Some(anyhow::anyhow!("Final send failed")),
        });
        assert!(shutdown_error.unwrap().contains("Capture could not stop"));
        assert!(transcript_incomplete);
    }
}
