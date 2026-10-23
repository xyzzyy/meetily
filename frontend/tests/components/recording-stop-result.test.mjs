import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';

/** Run the actual stop callback with its command and UI boundaries replaced. */
function loadStopAction(result) {
  const source = fs.readFileSync(new URL('../../src/components/RecordingControls.tsx', import.meta.url), 'utf8');
  const file = ts.createSourceFile('RecordingControls.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let initializer;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(file) === 'stopRecordingAction') {
      initializer = node.initializer.getText(file);
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
  assert.ok(initializer);
  const calls = { stopped: [], success: 0, processing: [], stopping: [], paths: [] };
  const compiled = ts.transpileModule(`const stop = ${initializer}; stop;`, {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText;
  const action = vm.runInNewContext(compiled, {
    useCallback: fn => fn,
    invoke: async () => { if (result instanceof Error) throw result; return result; },
    appDataDir: async () => '/data',
    setIsProcessing: value => calls.processing.push(value),
    setIsStopping: value => calls.stopping.push(value),
    setRecordingPath: value => calls.paths.push(value),
    onRecordingStop: value => calls.stopped.push(value),
    Analytics: { trackTranscriptionSuccess: () => calls.success++ },
    console: { log() {}, error() {} },
  });
  return { action, calls };
}

test('a duplicate stop does not start post-processing or report success', async () => {
  const { action, calls } = loadStopAction(false);
  await action();
  assert.deepEqual(calls.stopped, []);
  assert.equal(calls.success, 0);
  assert.deepEqual(calls.paths, []);
  assert.equal(calls.processing.at(-1), false);
  assert.equal(calls.stopping.at(-1), false);
});

test('the owning stop starts post-processing without claiming transcription success', async () => {
  const { action, calls } = loadStopAction(true);
  await action();
  assert.deepEqual(calls.stopped, [true]);
  assert.equal(calls.success, 0);
});

test('a failed stop cannot report success', async () => {
  const { action, calls } = loadStopAction(new Error('Write failed'));
  await action();
  assert.deepEqual(calls.stopped, [false]);
  assert.equal(calls.success, 0);
});
