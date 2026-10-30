import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
const source=fs.readFileSync(new URL('../../src/components/MeetingDetails/TranscriptButtonGroup.tsx',import.meta.url),'utf8');
const ast=ts.createSourceFile('TranscriptButtonGroup.tsx',source,ts.ScriptTarget.Latest,true,ts.ScriptKind.TSX);
const stateNames=[];
function visit(node) {
  if(ts.isVariableDeclaration(node)&&ts.isArrayBindingPattern(node.name)&&node.initializer&&ts.isCallExpression(node.initializer)
    &&node.initializer.expression.getText(ast)==='useState') stateNames.push(node.name.elements[0].name.getText(ast));
  ts.forEachChild(node,visit);
}
visit(ast);
function render(id) {
  let cursor=0, refreshed=0;
  const calls=[];
  const states=new Map();
  const jsx=(type,props)=>({type,props:props||{}});
  const require=name=>{
    if(name==='react')return {useState(initial){const key=stateNames[cursor++];if(!states.has(key))states.set(key,key==='undoReviewId'?id:initial);return [states.get(key),value=>states.set(key,value)];},useCallback:fn=>fn,useEffect(){}};
    if(name==='react/jsx-runtime')return {jsx,jsxs:jsx};
    if(name==='@/contexts/ConfigContext')return {useConfig:()=>({betaFeatures:{importAndRetranscribe:false}})};
    if(name==='@tauri-apps/api/core')return {invoke:async(command,args)=>{calls.push({command,args});return null;}};
    if(name==='sonner')return {toast:{success(){},warning(){},error(){}}};
    return new Proxy({},{get:(_,key)=>String(key)});
  };
  const module={exports:{}};
  vm.runInNewContext(ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.CommonJS,jsx:ts.JsxEmit.ReactJSX}}).outputText,
    {require,module,exports:module.exports});
  const tree=module.exports.TranscriptButtonGroup({transcriptCount:3,onCopyTranscript(){},onOpenMeetingFolder:async()=>{},meetingId:'meeting',meetingFolderPath:'/audio',onRefetchTranscripts:async()=>{refreshed++;}});
  return {tree,calls,refreshed:()=>refreshed};
}
function nodes(node){if(!node||typeof node!=='object')return [];if(Array.isArray(node))return node.flatMap(nodes);return [node,...nodes(node.props?.children)];}
assert.equal(nodes(render(null).tree).some(n=>n.props.title==='Undo last enhancement'),false);
const instance=render('run-1');
const undo=nodes(instance.tree).find(n=>n.props.title==='Undo last enhancement');
assert.ok(undo,'Undo remains available even when the beta Enhance setting is disabled');
await undo.props.onClick();
assert.equal(instance.calls[0].command,'undo_enhancement');
assert.equal(instance.calls[0].args.meetingId,'meeting');
assert.equal(instance.calls[0].args.reviewId,'run-1');
assert.equal(instance.refreshed(),1);
console.log('Enhance Undo control checks passed');
