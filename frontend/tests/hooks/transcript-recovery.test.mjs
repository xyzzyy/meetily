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
const require = name => {
  if (name === 'react') return { useState: value => [value, () => {}], useCallback: fn => fn };
  if (name === '@tauri-apps/api/core') return { invoke: async () => undefined };
  if (name === '@/services/indexedDBService') return { indexedDBService: {
    getMeetingMetadata: async () => ({ title: 'Recovered meeting' }), getTranscripts: async () => [row], markMeetingSaved: async () => {},
  } };
  if (name === '@/services/storageService') return { storageService: { saveMeeting: async (...args) => {
    saved.push(args); return { meeting_id: 'restored' };
  } } };
  if (name === '@/lib/summary-language-preferences') return { applyPinnedSummaryLanguageToMeeting: async () => {} };
  return {};
};
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } });
const module = { exports: {} };
vm.runInNewContext(compiled.outputText, { module, exports: module.exports, require, console });
const result = await module.exports.useTranscriptRecovery().recoverMeeting('meeting');
assert.equal(result.success, true);
const restored = saved[0][1][0];
for (const field of ['speaker_id', 'speaker_label', 'speaker_profile_id', 'speaker_source', 'speaker_color',
  'is_overlap', 'candidate_speaker_ids', 'diarization_status', 'diarization_method', 'diarization_confidence']) {
  assert.deepEqual(restored[field], row[field], `Recovery lost ${field}`);
}
console.log('Transcript recovery preserves speaker metadata: passed');
