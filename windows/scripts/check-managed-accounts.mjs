import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

// Exercise the shipped renderer's account selection, not a duplicate model.
const html = readFileSync(new URL('../codenotch/ui/notch.html', import.meta.url), 'utf8');
const selection = html.slice(html.indexOf('function providers(){'), html.indexOf('// Antigravity\'s own'));
const glyph = html.slice(html.indexOf('function glyphHtml('), html.indexOf('// Provider table'));
const absent = {status:'absent', windows:[]};
const state = {
  secondAccounts:{}, notchSlots:null,
  usage:{status:'ok',windows:[]}, codexSnap:{status:'ok',windows:[]},
  cursorSnap:absent,agSnap:absent,grokSnap:absent,openCodeSnap:absent,glmSnap:absent,
  devinSnap:absent,commandCodeSnap:absent,kimiSnap:absent,copilotSnap:absent,kiroSnap:absent,
};
const ids = () => Array.from(vm.runInNewContext(selection+';providers().map(p=>p.id)',state));
assert.deepEqual(ids(),['claude','codex']);
state.secondAccounts = {
  'codex-managed-1':{provider:'codex',label:'Personal',snapshot:{status:'ok',windows:[]},disconnected:false},
  'codex-managed-2':{provider:'codex',label:'Work',snapshot:{status:'ok',windows:[]},disconnected:false},
};
assert.deepEqual(ids(),['claude','codex-managed-1','codex-managed-2']);
state.secondAccounts['codex-managed-1'].disconnected=true;
assert.deepEqual(ids(),['claude','codex-managed-2']);
state.secondAccounts['codex-managed-2'].disconnected=true;
assert.deepEqual(ids(),['claude'],'Hidden registered accounts must not expose the default CLI account');
state.secondAccounts={};
assert.deepEqual(ids(),['claude','codex'],'Only removing registration restores the legacy source');
for(const family of ['claude','codex']){
  const ctx={glyphs:{[family]:{kind:'svg',svg:'<svg fill="currentColor"><path fill="#D97757"/></svg>'}},
    BRAND_TINT:{codex:'#ffffff',claude:'#D97757'},uniqueSvgIds:s=>s};
  const one=vm.runInNewContext(glyph+`;glyphHtml({id:'${family}-managed-1',base:'${family}',badge:'1'})`,ctx);
  const two=vm.runInNewContext(glyph+`;glyphHtml({id:'${family}-managed-2',base:'${family}',badge:'2'})`,ctx);
  assert.notEqual(one,two,`${family} account icons need different shades`);
}
console.log('Managed renderer: registration, disconnect, fallback and two icon tones passed');
