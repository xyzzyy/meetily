// Run with: node --test frontend/tests/components/model-deletion.test.mjs
// Tauri calls are mocked. No installed models or app data are accessed.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const require = createRequire(import.meta.url);
const ts = require(process.env.TYPESCRIPT_PATH || 'typescript');
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const cases = [
  ['BuiltInModelManager', 'BuiltInModelManager', 'summary', 'builtin_ai_delete_model'],
  ['WhisperModelManager', 'ModelManager', 'small', 'whisper'],
  ['ParakeetModelManager', 'ParakeetModelManager', 'parakeet-tdt-0.6b-v3-int8', 'parakeet'],
];

function manager([file, component, name, command], options = {}) {
  const sourcePath = path.join(root, `src/components/${file}.tsx`);
  const source = fs.readFileSync(sourcePath, 'utf8');
  const ast = ts.createSourceFile(sourcePath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const stateNames = [];
  function visit(node) {
    if (ts.isVariableDeclaration(node) && ts.isArrayBindingPattern(node.name)
        && ts.isCallExpression(node.initializer)
        && node.initializer.expression.getText(ast) === 'useState') {
      stateNames.push(node.name.elements[0].name.getText(ast));
    }
    ts.forEachChild(node, visit);
  }
  visit(ast);
  const model = { name, display_name: name, size_mb: 100, context_size: 100,
    status: file === 'BuiltInModelManager' ? { type: 'available' } : 'Available' };
  const states = new Map(Object.entries({ models: [model], loading: false, isLoading: false,
    initialized: true, hasFetched: true }));
  const refs = [];
  let stateCursor = 0;
  let refCursor = 0;
  const calls = [];
  const notices = [];
  const selections = [];
  const dialogs = [];
  const jsx = (type, props) => ({ type, props: props || {} });
  const hooks = {
    useState(initial) {
      const key = stateNames[stateCursor++];
      if (!states.has(key)) states.set(key, typeof initial === 'function' ? initial() : initial);
      return [states.get(key), value => states.set(key,
        typeof value === 'function' ? value(states.get(key)) : value)];
    },
    useRef(initial) {
      const index = refCursor++;
      return refs[index] ||= { current: initial };
    },
    useEffect() {}, useCallback: fn => fn,
  };
  const remove = async modelName => {
    calls.push({ command, modelName });
    await options.remove?.();
  };
  const api = { deleteCorruptedModel: remove, getAvailableModels: async () => [] };
  const mockRequire = id => {
    if (id === 'react') return hooks;
    if (id === 'react/jsx-runtime') return { jsx, jsxs: jsx, Fragment: 'Fragment' };
    if (id === '@tauri-apps/api/core') return { invoke: async (cmd, args) => {
      if (cmd === command) return remove(args.modelName);
      return [];
    } };
    if (id === '@tauri-apps/plugin-dialog') return { confirm: async (...args) => {
      dialogs.push(args);
      return options.confirm ? options.confirm() : true;
    } };
    if (id === 'sonner') return { toast: new Proxy({}, {
      get: (_, kind) => (...args) => notices.push({ kind, args }),
    }) };
    if (id === 'framer-motion') return { motion: new Proxy({}, { get: (_, key) => key }) };
    return new Proxy({ WhisperAPI: api, ParakeetAPI: api }, {
      get: (target, key) => target[key] || (() => ''),
    });
  };
  const compiled = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020, jsx: ts.JsxEmit.ReactJSX,
  } });
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, {
    exports: module.exports, module, require: mockRequire,
    console: { error() {} }, Set, Map, Date,
  });
  const render = () => {
    stateCursor = 0; refCursor = 0;
    return module.exports[component]({ selectedModel: options.unselected ? 'another-model' : name,
      onModelSelect: value => selections.push(value) });
  };
  function nodes(node) {
    if (!node || typeof node !== 'object') return [];
    if (Array.isArray(node)) return node.flatMap(nodes);
    return [node, ...nodes(node.props?.children)];
  }
  const click = () => {
    const control = nodes(render()).find(node => node.props.onDelete
      || node.props.title === `Delete ${name}`);
    assert.ok(control, `${component}: selected model must have a delete control`);
    return control.props.onDelete?.() ?? control.props.onClick?.({ stopPropagation() {} });
  };
  return { click, calls, notices, selections, dialogs };
}
const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };

for (const fixture of cases) {
  test(`${fixture[0]}: cancel keeps the model and selection`, async () => {
    const app = manager(fixture, { confirm: async () => false });
    app.click(); await settle();
    assert.equal(app.calls.length, 0);
    assert.deepEqual(app.selections, []);
    assert.equal(app.dialogs[0][1].okLabel, 'Delete');
  });
  test(`${fixture[0]}: confirm deletes once and clears only the selected model`, async () => {
    for (const unselected of [false, true]) {
      const app = manager(fixture, { unselected });
      app.click(); await settle();
      assert.deepEqual(app.calls, [{ command: fixture[3], modelName: fixture[2] }]);
      assert.deepEqual(app.selections, unselected ? [] : ['']);
    }
  });
  test(`${fixture[0]}: dialog and delete failures show an error and allow retry`, async () => {
    for (const phase of ['confirm', 'remove']) {
      let fail = true;
      const app = manager(fixture, { [phase]: async () => {
        if (fail) throw new Error('Test failure');
        return true;
      } });
      app.click(); await settle();
      assert.equal(app.notices.filter(n => n.kind === 'error').length, 1);
      assert.deepEqual(app.selections, []);
      if (phase === 'confirm') assert.equal(app.calls.length, 0);
      fail = false;
      app.click(); await settle();
      assert.deepEqual(app.selections, ['']);
    }
  });
  test(`${fixture[0]}: repeated clicks cannot queue dialogs or deletes`, async () => {
    let confirm, remove;
    const app = manager(fixture, {
      confirm: () => new Promise(resolve => { confirm = resolve; }),
      remove: () => new Promise(resolve => { remove = resolve; }),
    });
    app.click(); app.click();
    assert.equal(app.dialogs.length, 1);
    confirm(true); await settle();
    app.click();
    assert.equal(app.dialogs.length, 1);
    assert.equal(app.calls.length, 1);
    remove(); await settle();
    assert.deepEqual(app.selections, ['']);
  });
}
