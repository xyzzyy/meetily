import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';

/** Run the provider's real recovery cleanup with a delayed IndexedDB write. */
function loadMarkSaved() {
  const source = fs.readFileSync(new URL('../../src/contexts/TranscriptContext.tsx', import.meta.url), 'utf8');
  const file = ts.createSourceFile('TranscriptContext.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let initializer;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(file) === 'markMeetingAsSaved') {
      initializer = node.initializer.getText(file);
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
  assert.ok(initializer);
  let meetingId = 'old-recording';
  const storage = new Map([['indexeddb_current_meeting_id', meetingId]]);
  const saved = [];
  let finishSave;
  const compiled = ts.transpileModule(`const mark = ${initializer}; mark;`, {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText;
  const mark = vm.runInNewContext(compiled, {
    useCallback: fn => fn,
    currentMeetingId: meetingId,
    setCurrentMeetingId: value => { meetingId = typeof value === 'function' ? value(meetingId) : value; },
    indexedDBService: { markMeetingSaved: id => {
      saved.push(id);
      return new Promise(resolve => { finishSave = resolve; });
    } },
    sessionStorage: {
      getItem: key => storage.get(key), removeItem: key => storage.delete(key),
    },
    console: { error: () => assert.fail('Unexpected recovery cleanup error') },
  });
  return {
    mark, saved, storage,
    get meetingId() { return meetingId; },
    finish: () => finishSave(),
    startNew: () => {
      meetingId = 'new-recording';
      storage.set('indexeddb_current_meeting_id', meetingId);
    },
  };
}

test('completed recovery cleanup clears the saved recording ID', async () => {
  const state = loadMarkSaved();
  const marking = state.mark();
  state.finish();
  await marking;
  assert.deepEqual(state.saved, ['old-recording']);
  assert.equal(state.meetingId, null);
  assert.equal(state.storage.has('indexeddb_current_meeting_id'), false);
});

test('an old IndexedDB save preserves the new recording recovery ID', async () => {
  const state = loadMarkSaved();
  const marking = state.mark();
  state.startNew();
  state.finish();
  await marking;
  assert.deepEqual(state.saved, ['old-recording']);
  assert.equal(state.meetingId, 'new-recording');
  assert.equal(state.storage.get('indexeddb_current_meeting_id'), 'new-recording');
});
