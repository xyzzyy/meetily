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
  const calls = { stopped: [], processing: [], stopping: [], paths: [], requestsFinished: 0 };
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
    onStopRequestFinished: () => calls.requestsFinished++,
    console: { log() {}, error() {} },
  });
  return { action, calls };
}

test('a duplicate stop does not start post-processing', async () => {
  const { action, calls } = loadStopAction(false);
  await action();
  assert.deepEqual(calls.stopped, []);
  assert.deepEqual(calls.paths, []);
  assert.equal(calls.processing.at(-1), false);
  assert.equal(calls.stopping.at(-1), false);
  assert.equal(calls.requestsFinished, 1);
});

test('the owning stop starts post-processing', async () => {
  const { action, calls } = loadStopAction(true);
  await action();
  assert.deepEqual(calls.stopped, [true]);
  assert.equal(calls.requestsFinished, 1);
});

test('a failed stop reports a failed post-processing result', async () => {
  const { action, calls } = loadStopAction(new Error('Write failed'));
  await action();
  assert.deepEqual(calls.stopped, [false]);
  assert.equal(calls.requestsFinished, 1);
});

test('an inactive stop releases its request without starting post-processing', async () => {
  const { action, calls } = loadStopAction(new Error('No recording in progress'));
  await action();
  assert.deepEqual(calls.stopped, []);
  assert.equal(calls.requestsFinished, 1);
});

/** Run the component's actual event effect with controllable Tauri events. */
async function loadRecordingListeners() {
  const source = fs.readFileSync(new URL('../../src/components/RecordingControls.tsx', import.meta.url), 'utf8');
  const file = ts.createSourceFile('RecordingControls.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let effect;
  function visit(node) {
    if (ts.isCallExpression(node) && node.expression.getText(file) === 'useEffect'
      && node.arguments[0].getText(file).includes('Setting up recording event listeners')) {
      effect = node.getText(file);
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
  assert.ok(effect);
  const listeners = new Map();
  const calls = { stopped: [] };
  const compiled = ts.transpileModule(effect, {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText;
  let cleanup;
  vm.runInNewContext(compiled, {
    useEffect: fn => { cleanup = fn(); },
    listen: async (event, listener) => {
      listeners.set(event, listener);
      return () => listeners.delete(event);
    },
    setTranscriptionErrors() {}, setIsProcessing() {}, setSpeechDetected() {},
    onRecordingStop: value => calls.stopped.push(value),
    onTranscriptionError() {},
    console: { log() {}, error() {} },
  });
  await new Promise(setImmediate);
  return { listeners, calls, cleanup };
}

test('a fatal worker error waits for native stop completion before post-processing', async t => {
  const { listeners, calls, cleanup } = await loadRecordingListeners();
  t.after(cleanup);
  listeners.get('transcription-error')({ payload: {
    phase: 'active', actionable: true, error: 'Engine initialization failed',
    userMessage: 'Speech recognition failed. Recording is stopping.',
  } });
  assert.deepEqual(calls.stopped, []);
});

test('a nonfatal chunk error does not end the recording in the interface', async t => {
  const { listeners, calls, cleanup } = await loadRecordingListeners();
  t.after(cleanup);
  listeners.get('transcription-error')({ payload: {
    phase: 'active', actionable: false, error: 'One chunk failed', userMessage: 'One chunk failed',
  } });
  assert.deepEqual(calls.stopped, []);
});
