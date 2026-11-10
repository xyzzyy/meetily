// Run with: bun test frontend/tests/components/speaker-attribute.test.mjs
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const ts = require(process.env.TYPESCRIPT_PATH || 'typescript');
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const sourcePath = path.join(root, 'src/components/MeetingDetails/SpeakerAttributeButton.tsx');
const compiled = ts.transpileModule(fs.readFileSync(sourcePath, 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020, jsx: ts.JsxEmit.ReactJSX },
});

function makeButton(profiles) {
  const states = [];
  let cursor = 0;
  const jsx = (type, props) => ({ type, props: props || {} });
  const mockRequire = name => {
    if (name === 'react') return {
      useState(initial) {
        const index = cursor++;
        if (states.length <= index) states.push(initial);
        return [states[index], value => { states[index] = value; }];
      },
    };
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx, Fragment: 'Fragment' };
    return new Proxy({}, { get: (_, key) => String(key) });
  };
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, { exports: module.exports, module, require: mockRequire });
  const attributed = [];
  const speaker = { speakerId: 'speaker-1', speakerLabel: 'Speaker 1', segmentCount: 4 };
  const render = () => {
    cursor = 0;
    return module.exports.SpeakerAttributeButton({
      speaker, profiles, disabled: false, busy: false,
      onAttribute: (target, attribution) => attributed.push({ target, attribution }),
    });
  };
  const type = query => { render(); states[1] = query; };
  return { render, type, attributed, states };
}

function nodes(node) {
  if (!node || typeof node !== 'object') return [];
  if (Array.isArray(node)) return node.flatMap(nodes);
  return [node, ...nodes(node.props?.children)];
}
function text(node) {
  if (Array.isArray(node)) return node.map(text).join('');
  if (node === null || node === undefined || typeof node === 'boolean') return '';
  if (typeof node !== 'object') return String(node);
  return text(node.props?.children);
}
const items = tree => nodes(tree).filter(node => node.type === 'CommandItem');

const madior = { id: 'profile-1', name: 'Scrum Master Madior', modelVersion: 'v1', sampleCount: 3, speechSeconds: 30 };

// Remembered voices are listed, and choosing one attributes by id.
{
  const button = makeButton([madior]);
  const [voice] = items(button.render());
  assert.ok(text(voice).includes('Scrum Master Madior'));
  voice.props.onSelect();
  assert.equal(JSON.stringify(button.attributed[0].attribution), JSON.stringify({ profileId: 'profile-1' }));
  assert.equal(button.attributed[0].target.speakerId, 'speaker-1');
}
// A new name offers to remember a new person, and attributes by name.
{
  const button = makeButton([madior]);
  button.type('  Alice ');
  const offer = items(button.render()).find(item => text(item).includes('as a new person'));
  assert.ok(offer, 'A new name must offer to remember a new person');
  assert.ok(text(offer).includes('Alice'));
  offer.props.onSelect();
  assert.equal(JSON.stringify(button.attributed[0].attribution), JSON.stringify({ name: 'Alice' }));
}
// Typing an existing name, in any case, never offers a duplicate person.
{
  const button = makeButton([madior]);
  button.type('scrum master MADIOR');
  assert.equal(items(button.render()).some(item => text(item).includes('as a new person')), false);
}
// With no remembered voices and nothing typed, the list explains what to do.
{
  const button = makeButton([]);
  const tree = button.render();
  assert.equal(items(tree).length, 0);
  assert.ok(text(tree).includes('Type a name to remember this speaker'));
}
console.log('Speaker attribution picker: all checks passed');

// An older voiceprint can still name a speaker, but it is not shown as ready
// for automatic recognition after the feature recipe changes.
{
  const button = makeButton([{ ...madior, recognitionReady: false }]);
  const [voice] = items(button.render());
  assert.ok(text(voice).includes('Name only'));
  voice.props.onSelect();
  assert.equal(JSON.stringify(button.attributed[0].attribution), JSON.stringify({ profileId: madior.id }));
}
