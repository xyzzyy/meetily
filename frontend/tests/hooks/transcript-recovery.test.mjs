import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
const source = fs.readFileSync(new URL('../../src/hooks/useTranscriptRecovery.ts', import.meta.url), 'utf8');
const saved = [];
const row = { id: 1, text: 'Hello', timestamp: '00:01', confidence: 0.9, sequenceId: 1,
  audio_start_time: 1, audio_end_time: 2, duration: 1, speaker_id: null, speaker_label: 'Multiple speakers',
  speaker_profile_id: null, speaker_source: 'mixed', speaker_color: '#ea580c', is_overlap: true,
  candidate_speaker_ids: ['alice', 'bob'], diarization_status: 'provisional',
  diarization_method: 'live_source_wespeaker', diarization_confidence: 0.7 };
const calls = [];
const makeRequire = metadata => name => {
  if (name === 'react') return { useState: value => [value, () => {}], useCallback: fn => fn };
  if (name === '@tauri-apps/api/core') return { invoke: async command => { calls.push(command); return undefined; } };
  if (name === '@/services/indexedDBService') return { indexedDBService: {
    getMeetingMetadata: async () => metadata, getTranscripts: async () => [row], markMeetingSaved: async () => {},
  } };
  if (name === '@/services/storageService') return { storageService: { saveMeeting: async (...args) => {
    saved.push(args); return { meeting_id: 'restored' };
  } } };
  if (name === '@/lib/summary-language-preferences') return { applyPinnedSummaryLanguageToMeeting: async () => {} };
  return {};
};
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } });
const recover = metadata => {
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, { module, exports: module.exports, require: makeRequire(metadata), console });
  return module.exports.useTranscriptRecovery().recoverMeeting('meeting');
};
const result = await recover({ title: 'Recovered meeting' });
assert.equal(result.success, true);
const restored = saved[0][1][0];
for (const field of ['speaker_id', 'speaker_label', 'speaker_profile_id', 'speaker_source', 'speaker_color',
  'is_overlap', 'candidate_speaker_ids', 'diarization_status', 'diarization_method', 'diarization_confidence']) {
  assert.deepEqual(restored[field], row[field], `Recovery lost ${field}`);
}
console.log('Transcript recovery preserves speaker metadata: passed');

// A meeting saved without a folder must never borrow the active recording's folder.
calls.length = 0;
const withoutFolder = await recover({ title: 'Recovered meeting' });
assert.equal(withoutFolder.success, true);
assert.equal(calls.includes('get_meeting_folder_path'), false, 'Recovery must not ask for the active recording folder');
assert.equal(calls.includes('recover_audio_from_checkpoints'), false);
assert.equal(calls.includes('cleanup_checkpoints'), false);

// A saved folder is still used for audio recovery and cleanup.
calls.length = 0;
await recover({ title: 'Recovered meeting', folderPath: '/recordings/Meeting' });
assert.equal(calls.includes('recover_audio_from_checkpoints'), true);
assert.equal(calls.includes('cleanup_checkpoints'), true);
assert.equal(calls.includes('get_meeting_folder_path'), false);
console.log('Transcript recovery only uses the folder saved with the meeting: passed');
