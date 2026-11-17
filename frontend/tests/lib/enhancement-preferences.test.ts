import { afterAll, beforeEach, expect, mock, test } from 'bun:test';
const events = { ...await import('@tauri-apps/api/event') };
const handlers = new Map<string, (event: any) => void>();
mock.module('@tauri-apps/api/event', () => ({ ...events, listen: async (name: string, handler: any) => { handlers.set(name, handler); return () => handlers.delete(name); } }));
const core = { ...await import('@tauri-apps/api/core') };
const invoke = mock(async (_command: string, _args?: any): Promise<any> => ({ enabled: true, refinementSupported: true }));
mock.module('@tauri-apps/api/core', () => ({ ...core, invoke }));
const { readEnhancementPreferences, writeEnhancementPreferences, startAutoEnhancement, waitForAutoEnhancement } = await import('../../src/lib/enhancement-preferences');
const values = new Map<string, string>();
beforeEach(() => {
  values.clear(); invoke.mockClear();
  invoke.mockImplementation(async command => command === 'get_retranscription_outcome' ? 'running' : command.endsWith('_get_available_models')
    ? [{ name: 'model', status: 'Available' }, { name: 'nemotron-q2', status: 'Available' }]
    : { enabled: true, postCallRefinementEnabled: true, refinementSupported: true });
  (globalThis as any).window = { sessionStorage: { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) }, localStorage: { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value) } };
});
afterAll(() => { mock.module('@tauri-apps/api/event', () => events); delete (globalThis as any).window; mock.module('@tauri-apps/api/core', () => core); });

test('auto enhance is off until enabled and malformed settings are safe', async () => {
  expect(readEnhancementPreferences().autoEnhance).toBe(false);
  expect(await startAutoEnhancement('meeting', '/audio')).toBe(false);
  expect(invoke).not.toHaveBeenCalled();
  values.set('enhancementPreferences', '{');
  expect(readEnhancementPreferences()).toEqual({ autoEnhance: false, provider: '', model: '' });
});

test('a separate Nemotron model persists and starts on saved audio', async () => {
  const settings = { autoEnhance: true, provider: 'nemotron' as const, model: 'nemotron-q2' };
  writeEnhancementPreferences(settings);
  expect(readEnhancementPreferences()).toEqual(settings);
  expect(await startAutoEnhancement('meeting', '/audio')).toBe(true);
  expect(invoke).toHaveBeenCalledWith('start_retranscription_command', {
    meetingId: 'meeting', meetingFolderPath: '/audio', provider: 'nemotron', model: 'nemotron-q2', language: null, assignSpeakers: true, preserveSpeakers: true,
  });
  expect(invoke.mock.calls.some(([command]) => command === 'api_save_transcript_config')).toBe(false);
});

test('missing audio and missing model do not start enhancement', async () => {
  writeEnhancementPreferences({ autoEnhance: true, provider: 'nemotron', model: 'model' });
  await expect(startAutoEnhancement('meeting', null)).rejects.toThrow('no saved audio');
  writeEnhancementPreferences({ autoEnhance: true, provider: '', model: '' });
  await expect(startAutoEnhancement('meeting', '/audio')).rejects.toThrow('Select a default');
  expect(invoke).not.toHaveBeenCalled();
});

test('Parakeet uses auto language and skips new speaker analysis on unsupported devices', async () => {
  values.set('primaryLanguage', 'fr');
  writeEnhancementPreferences({ autoEnhance: true, provider: 'parakeet', model: 'model' });
  invoke.mockImplementation(async command => command.endsWith('_get_available_models') ? [{ name: 'model', status: 'Available' }]
    : command === 'get_diarization_settings' ? { enabled: true, postCallRefinementEnabled: true } : { refinementSupported: false });
  await startAutoEnhancement('meeting', '/audio');
  expect(invoke).toHaveBeenLastCalledWith('start_retranscription_command', {
    meetingId: 'meeting', meetingFolderPath: '/audio', provider: 'parakeet', model: 'model', language: null, assignSpeakers: false, preserveSpeakers: true,
  });
});

for (const event of ['retranscription-complete', 'retranscription-error']) {
  test(`auto summary waits until ${event} and can then use the saved transcript`, async () => {
    writeEnhancementPreferences({ autoEnhance: true, provider: 'nemotron', model: 'model' });
    await startAutoEnhancement('pending-meeting', '/audio');
    let ready = false;
    const waiting = waitForAutoEnhancement('pending-meeting').then(() => { ready = true; });
    await Promise.resolve();
    expect(ready).toBe(false);
    handlers.get(event)!({ payload: { meeting_id: 'other-meeting' } });
    await Promise.resolve();
    expect(ready).toBe(false);
    handlers.get(event)!({ payload: { meeting_id: 'pending-meeting' } });
    await waiting;
    expect(ready).toBe(true);
  });
}

test('an unavailable model skips enhancement and releases the summary wait', async () => {
  writeEnhancementPreferences({ autoEnhance: true, provider: 'nemotron', model: 'missing-model' });
  expect(await startAutoEnhancement('missing-model-meeting', '/audio')).toBe(false);
  await waitForAutoEnhancement('missing-model-meeting');
  expect(invoke.mock.calls.some(([command]) => command === 'start_retranscription_command')).toBe(false);
});

test('automatic speaker assignment respects the post-call switch', async () => {
  writeEnhancementPreferences({ autoEnhance: true, provider: 'nemotron', model: 'model' });
  invoke.mockImplementation(async command => command.endsWith('_get_available_models') ? [{ name: 'model', status: 'Available' }]
    : { enabled: true, postCallRefinementEnabled: false });
  await startAutoEnhancement('no-speaker-refinement', '/audio');
  expect(invoke).toHaveBeenLastCalledWith('start_retranscription_command', {
    meetingId: 'no-speaker-refinement', meetingFolderPath: '/audio', provider: 'nemotron', model: 'model', language: null, assignSpeakers: false, preserveSpeakers: true,
  });
});

for (const running of [true, false]) {
  test(`a reload checks the native meeting state (running: ${running})`, async () => {
    values.set('autoEnhancement:restored-meeting', 'pending');
    invoke.mockImplementation(async () => running ? 'running' : 'completed');
    let ready = false;
    const waiting = waitForAutoEnhancement('restored-meeting').then(() => { ready = true; });
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(invoke).toHaveBeenCalledWith('get_retranscription_outcome', { meetingId: 'restored-meeting' });
    if (running) {
      expect(ready).toBe(false);
      handlers.get('retranscription-complete')!({ payload: { meeting_id: 'restored-meeting' } });
    }
    await waiting;
    expect(ready).toBe(true);
    expect(values.has('autoEnhancement:restored-meeting')).toBe(false);
  });
}

for (const restored of [false, true]) {
  test(`a later enhancement error runs the speaker fallback (restored: ${restored})`, async () => {
    const meetingId = `failed-enhancement-${restored}`;
    if (restored) values.set(`autoEnhancement:${meetingId}`, 'pending');
    else {
      writeEnhancementPreferences({ autoEnhance: true, provider: 'nemotron', model: 'model' });
      await startAutoEnhancement(meetingId, '/audio');
    }
    const waiting = waitForAutoEnhancement(meetingId);
    await new Promise(resolve => setTimeout(resolve, 0));
    handlers.get('retranscription-error')!({ payload: { meeting_id: meetingId, error: 'Model load failed' } });
    await waiting;
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(invoke).toHaveBeenCalledWith('start_final_diarization', { request: { meetingId, overwriteManualEdits: false } });
  });
}

for (const message of ['Retranscription cancelled', 'Final speaker refinement was cancelled', 'Enhancement cancelled. The saved transcript was kept.']) {
  test(`explicit cancellation skips the fallback: ${message}`, async () => {
    writeEnhancementPreferences({ autoEnhance: true, provider: 'nemotron', model: 'model' });
    await startAutoEnhancement('cancelled-meeting', '/audio');
    const waiting = waitForAutoEnhancement('cancelled-meeting');
    handlers.get('retranscription-error')!({ payload: { meeting_id: 'cancelled-meeting', error: message, cancelled: true } });
    await waiting;
    expect(invoke.mock.calls.some(([command]) => command === 'start_final_diarization')).toBe(false);
  });
}
for (const outcome of ['failed', 'cancelled', 'completed']) {
  test(`a missed terminal event restores outcome ${outcome}`, async () => {
    values.set('autoEnhancement:missed-event', 'pending');
    invoke.mockImplementation(async command => command === 'get_retranscription_outcome' ? outcome
      : { enabled: true, postCallRefinementEnabled: true, refinementSupported: true });
    await waitForAutoEnhancement('missed-event');
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(invoke.mock.calls.some(([command]) => command === 'start_final_diarization')).toBe(outcome === 'failed');
  });
}
