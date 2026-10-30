import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
const source = fs.readFileSync(new URL('../../src/lib/transcript-comparison.ts', import.meta.url), 'utf8');
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } });
const module = { exports: {} };
vm.runInNewContext(compiled.outputText, { module, exports: module.exports });
const compare = module.exports.compareTranscriptText;
for (const [before, after] of [['Andrew, feel free to drop or stay.', 'Enter feel free to drop.'],
  ['thank you', 'thank thank you'], ['', 'hello'], ['hello', ''], ['  hello\n\nworld ', 'hello world'],
  [Array(2000).fill('old ').join(''), Array(2000).fill('new ').join('')]]) {
  const parts = compare(before, after);
  assert.equal(parts.before.map(p => p.text).join(''), before);
  assert.equal(parts.after.map(p => p.text).join(''), after);
}
const diff = compare('Andrew feel free to drop or stay', 'Enter feel free to drop');
assert.match(diff.before.filter(p => p.changed).map(p => p.text).join(''), /Andrew/);
assert.match(diff.before.filter(p => p.changed).map(p => p.text).join(''), /or stay/);
assert.match(diff.after.filter(p => p.changed).map(p => p.text).join(''), /Enter/);
assert.equal(diff.after.find(p => p.text.trim() === 'free').changed, false);
console.log('Transcript comparison checks passed');
