import type { Page } from '@playwright/test';

export const transcriptText = 'We agreed to review the meeting notes on Friday.';
export const summaryText = 'Review the meeting notes. [00:00-00:04]';
export const meetingId = 'walkthrough-meeting';

// A stateful Tauri boundary for one normal session. This code is loaded only by
// Playwright; it is never imported by src/ or included in a user's app.
export async function installUsageDesktop(page: Page) {
  await page.addInitScript(({ transcriptText, summaryText, meetingId }) => {
    type Callback = (event: { event: string; id: number; payload: unknown }) => void;
    const callbacks = new Map<number, Callback>();
    const listeners = new Map<number, { event: string; handler: number }>();
    let nextId = 0;
    const key = 'meetily-usage-fixture';
    const state = JSON.parse(localStorage.getItem(key) || 'null') || {
      recording: false, paused: false, title: '', meeting: null, summary: null,
      history: [], calls: [], unexpected: [],
    };
    const save = () => localStorage.setItem(key, JSON.stringify(state));
    const emit = (event: string, payload: unknown = null) => {
      for (const [id, listener] of listeners) {
        if (listener.event === event) callbacks.get(listener.handler)?.({ event, id, payload });
      }
    };
    const store = new Map<string, unknown>([
      ['analyticsOptedIn', false], ['analyticsDefaultOffMigrationV1', true],
    ]);
    const metadata = () => {
      if (!state.meeting) throw new Error(`Meeting not found: ${meetingId}`);
      return {
        id: meetingId, title: state.meeting.title, folder_path: null,
        created_at: '2026-10-28T10:00:00Z', updated_at: '2026-10-28T10:00:00Z',
      };
    };
    const fixture = {
      emitTranscript() {
        if (!state.recording || state.paused) throw new Error('Recording must be active.');
        const segment = {
          id: 'transcript-1', text: transcriptText, timestamp: '10:00:00',
          sequence_id: 1, chunk_start_time: 0, is_partial: false,
          audio_start_time: 0, audio_end_time: 4, duration: 4, confidence: 0.98,
        };
        state.history.push(segment);
        save();
        emit('transcript-update', segment);
      },
    };
    Object.assign(window, {
      __meetilyUsageFixture: fixture,
      __TAURI_OS_PLUGIN_INTERNALS__: {
        platform: 'macos', version: '15.0', family: 'unix', os_type: 'macos',
        arch: 'aarch64', eol: '\n', exe_extension: '',
      },
      __TAURI_EVENT_PLUGIN_INTERNALS__: {
        unregisterListener(_event: string, id: number) { listeners.delete(id); },
      },
      __TAURI_INTERNALS__: {
        metadata: {
          currentWindow: { label: 'main' },
          currentWebview: { label: 'main', windowLabel: 'main' },
        },
        transformCallback(callback: Callback) { callbacks.set(++nextId, callback); return nextId; },
        unregisterCallback(id: number) { callbacks.delete(id); },
        convertFileSrc(file: string) { return file; },
        async invoke(command: string, args: Record<string, any> = {}) {
          state.calls.push({ command, args });
          save();
          switch (command) {
            case 'plugin:event|listen': {
              const id = ++nextId;
              listeners.set(id, { event: args.event, handler: args.handler });
              return id;
            }
            case 'plugin:event|unlisten': listeners.delete(args.eventId); return null;
            case 'plugin:event|emit': emit(args.event, args.payload); return null;
            case 'plugin:window|set_theme':
            case 'set_language_preference':
            case 'plugin:store|save':
            case 'plugin:resources|close':
            case 'whisper_init': return null;
            case 'plugin:store|load': return 1;
            case 'plugin:store|has': return store.has(args.key);
            case 'plugin:store|get': return [store.get(args.key), store.has(args.key)];
            case 'plugin:store|set': store.set(args.key, args.value); return null;
            case 'plugin:path|resolve_directory': return '/fixture/Meetily';
            case 'get_onboarding_status': return {
              completed: true, current_step: 1,
              model_status: { parakeet: 'downloaded', summary: 'downloaded' },
            };
            case 'check_first_launch': return false;
            case 'plugin:app|version': return '0.4.1';
            case 'api_get_transcript_config': return { provider: 'localWhisper', model: 'large-v3', apiKey: null };
            case 'api_get_model_config': return { provider: 'ollama', model: 'fixture-model', whisperModel: 'large-v3' };
            case 'get_ollama_models': return [{ name: 'fixture-model' }];
            case 'get_meeting_folder_path':
            case 'api_get_api_key':
            case 'get_meeting_diarization_status': return null;
            case 'get_recording_preferences': return { preferred_mic_device: null, preferred_system_device: null };
            case 'get_diarization_settings': return { enabled: false, mode: 'off', showProvisionalLabels: false };
            case 'get_diarization_model_status': return { installed: false, refinementSupported: false };
            case 'get_audio_devices': return [{ name: 'Fixture microphone', device_type: 'Input' }];
            case 'whisper_has_available_models': return true;
            case 'is_recording': return state.recording;
            case 'get_recording_state': return {
              is_recording: state.recording, is_paused: state.paused,
              is_active: state.recording && !state.paused, recording_duration: 4, active_duration: 4,
            };
            case 'get_recording_meeting_name': return state.title;
            case 'get_transcript_history': return state.history;
            case 'get_transcription_status': return { chunks_in_queue: 0, is_processing: false, last_activity_ms: 0 };
            case 'start_recording_with_devices_and_meeting':
              state.recording = true;
              state.title = args.meetingName;
              state.history = [];
              save();
              emit('recording-started');
              return null;
            case 'pause_recording': state.paused = true; save(); emit('recording-paused'); return null;
            case 'resume_recording': state.paused = false; save(); emit('recording-resumed'); return null;
            case 'stop_recording':
              state.recording = false;
              save();
              emit('recording-stopped', { message: 'Recording stopped', meeting_name: state.title });
              return true;
            case 'api_save_transcript':
              state.meeting = { title: args.meetingTitle, transcripts: args.transcripts };
              save();
              return { meeting_id: meetingId };
            case 'api_get_meetings': return state.meeting ? [{ id: meetingId, title: state.meeting.title }] : [];
            case 'api_get_meeting_metadata': return metadata();
            case 'api_get_meeting': return { ...metadata(), transcripts: state.meeting.transcripts };
            case 'api_get_meeting_transcripts': {
              const transcripts = state.meeting?.transcripts ?? [];
              const offset = args.offset ?? 0;
              const limit = args.limit ?? transcripts.length;
              return {
                transcripts: transcripts.slice(offset, offset + limit),
                has_more: offset + limit < transcripts.length, total_count: transcripts.length,
              };
            }
            case 'api_get_meeting_summary_language':
            case 'api_get_meeting_detected_summary_language':
            case 'api_save_meeting_detected_summary_language': return { language: null, storage: 'metadata' };
            case 'api_detect_transcript_summary_language': return { language: 'en', reason: 'detected' };
            case 'get_meeting_speakers':
            case 'list_speaker_profiles': return [];
            case 'api_list_templates': return [{ id: 'standard_meeting', name: 'Standard meeting', description: 'Meeting notes' }];
            case 'api_process_transcript':
              state.summary = { markdown: summaryText };
              save();
              return { process_id: 'fixture-summary-process' };
            case 'api_get_summary': return {
              meeting_id: meetingId, meetingName: state.meeting?.title,
              status: state.summary ? 'completed' : 'idle', data: state.summary,
              start: state.summary ? 'fixture-summary-process' : null, end: null, error: null,
            };
            case 'api_save_meeting_summary': state.summary = args.summary; save(); return null;
            case 'api_save_meeting_title':
              if (!state.meeting) throw new Error(`No meeting found with id ${meetingId}`);
              state.meeting.title = args.title; save(); return null;
            case 'api_search_transcripts': return state.meeting && transcriptText.includes(args.query)
              ? [{ id: meetingId, title: state.meeting.title, matchContext: transcriptText }] : [];
            case 'api_delete_meeting': state.meeting = null; state.summary = null; save(); return null;
            default:
              state.unexpected.push(command);
              save();
              throw new Error(`Unexpected desktop command: ${command}`);
          }
        },
      },
    });
  }, { transcriptText, summaryText, meetingId });
}

export async function readDesktopState(page: Page) {
  return page.evaluate(() => JSON.parse(localStorage.getItem('meetily-usage-fixture')!));
}
