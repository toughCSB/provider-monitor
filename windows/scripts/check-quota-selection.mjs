import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

// Evaluate the actual shipped picker, including its family and cadence helpers.
const html = readFileSync(new URL('../codenotch/ui/notch.html', import.meta.url), 'utf8');
const start = html.indexOf('function tightestOf(');
const end = html.indexOf('function staleOf(', start);
assert.ok(start >= 0 && end > start, 'quota selection functions were not found');
const source = html.slice(start, end);
const state = { agPrefs: { model: 'gemini' }, limitFor: () => 'automatic' };
const select = (windows, model, cadence = 'automatic') => {
  state.agPrefs.model = model;
  state.limitFor = () => cadence;
  return vm.runInNewContext(`${source};headlineOf({windows: ${JSON.stringify(windows)}}, 'gemini')`, state);
};
const gemini = { id: 'Gemini Models Weekly Limit', label: 'Weekly Limit', used: 0.2 };
const other = { id: 'Claude and GPT models Weekly Limit', label: 'Weekly Limit', used: 0.9 };
assert.equal(select([gemini, other], 'gemini').id, gemini.id);
assert.equal(select([gemini, other], '3p').id, other.id);
assert.equal(select([other], 'gemini'), null, 'missing Gemini family must show no reading');
assert.equal(select([gemini], '3p'), null, 'missing Claude/GPT family must show no reading');
assert.equal(select([other], 'gemini', '5h'), null, 'an explicit cadence cannot borrow another family');
assert.equal(select([{id:'requests',count:3}], 'gemini').count, 3, 'a generic request count is not a model quota');
console.log('AGY shipped JS quota selection: family isolation and missing-family cases passed');
