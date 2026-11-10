import assert from 'node:assert/strict';
import feedback, { requestFailureReason } from '../crates/aionui-ai-agent/src/manager/acp/pi_request_feedback.mjs';

let handler;
feedback({ on(name, callback) { assert.equal(name, 'agent_end'); handler = callback; } });
const notices = [];
const context = { ui: { notify(message, level) { notices.push({ message, level }); } } };
handler({ messages: [{ role: 'assistant', stopReason: 'error', errorMessage: 'HTTP 429: rate limit exceeded; secret request body' }] }, context);
assert.deepEqual(notices, [{ message: 'The model provider is rate limiting this request.', level: 'warning' }]);
handler({ messages: [{ role: 'assistant', stopReason: 'stop' }] }, context);
handler({ messages: [{ role: 'assistant', stopReason: 'aborted', errorMessage: 'aborted' }] }, context);
handler({ messages: [{ role: 'toolResult', errorMessage: 'tool error' }] }, context);
assert.equal(notices.length, 1);
assert.match(requestFailureReason('Free usage exceeded'), /free usage quota/);
assert.match(requestFailureReason('Insufficient credits'), /quota or credits/);
assert.match(requestFailureReason('HTTP 401 invalid API key'), /authentication failed/);
assert(!requestFailureReason('private request body sk-secret').includes('sk-secret'));
console.log('Pi feedback: failed assistant request emits a safe cause; success, abort and tool errors are ignored.');
