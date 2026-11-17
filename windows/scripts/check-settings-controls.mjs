import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const html = readFileSync(new URL('../codenotch/ui/settings.html', import.meta.url), 'utf8');
// Execute the shipped polling functions with a mocked Tauri bridge.
const start = html.indexOf('function noManagedLogin(');
const end = html.indexOf('reloadManaged().then(bootstrapManaged)', start);
assert.ok(start > 0 && end > start, 'Managed login polling code was not found');
const calls = [], errors = [];
const context = vm.createContext({
  managedPending: new Map(),
  invoke: async (command, args) => { calls.push([command, args]); throw new Error('No login in progress'); },
  errText: error => error.message || String(error),
  strip: message => errors.push(message),
  renderManaged: () => {},
});
vm.runInContext(html.slice(start, end), context);

await vm.runInContext('pollManaged()', context);
assert.equal(calls.length, 0, 'Idle polling must not call the backend');
await vm.runInContext('bootstrapManaged()', context);
assert.equal(calls.length, 4, 'Reopening settings probes the four slots once');
assert.deepEqual(errors, [], 'Expected idle errors must stay out of the red strip');

calls.length = 0;
context.invoke = async (command, args) => {
  calls.push([command, args]);
  if (args.provider === 'codex' && args.slot === 1) return { state: 'authenticating' };
  throw new Error('No login in progress');
};
await vm.runInContext('bootstrapManaged()', context);
assert.equal(context.managedPending.get('codex-1').state, 'authenticating');
calls.length = 0;
context.invoke = async (command, args) => { calls.push([command, args]); return { state: 'ready', identity: 'user@example.com' }; };
await vm.runInContext('pollManaged()', context);
assert.equal(calls.length, 1, 'Only an active login is polled');
assert.equal(context.managedPending.get('codex-1').state, 'ready');
calls.length = 0;
await vm.runInContext('pollManaged()', context);
assert.equal(calls.length, 0, 'Ready state waits for confirmation without polling');

let rejectRead;
const prior = { state: 'checking' };
context.managedPending.set('codex-1', prior);
context.invoke = () => new Promise((_, reject) => { rejectRead = reject; });
const pending = vm.runInContext('pollManaged()', context);
context.managedPending.delete('codex-1');
rejectRead(new Error('No login in progress'));
await pending;
assert.equal(context.managedPending.has('codex-1'), false, 'A cancelled login must not return from an in-flight read');
assert.deepEqual(errors, []);

context.managedPending.set('codex-1', { state: 'authenticating' });
context.invoke = async () => { throw new Error('unexpected bridge failure'); };
await vm.runInContext('pollManaged()', context);
assert.equal(errors.length, 1, 'Unexpected failures remain visible');
assert.match(errors[0], /unexpected bridge failure/);

console.log('Settings dropdown rules and managed login polling passed');
