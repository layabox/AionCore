// Regression for captured Windows OpenCode argv rejection. Synthetic ACP frames
// exercise the packaged integration; they do not claim vendor-wide compatibility.
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const { once } = require('node:events');
const fs = require('node:fs/promises');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');

const binary = path.resolve(process.argv[2]);
const reportPath = process.argv[3] && path.resolve(process.argv[3]);
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
let child;

(async () => {
  const temp = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), 'opencode-startup-')));
  const report = { scope: 'Captured argument rejection replay; synthetic ACP, no provider requests', cases: [] };
  const trace = path.join(temp, 'argv.jsonl');
  let logs = '';
  try {
    const script = path.join(temp, 'fixture.cjs');
    await fs.writeFile(script, `
      const fs = require('node:fs');
      const args = process.argv.slice(2);
      if (args.includes('--version')) { console.log('1.3.13'); process.exit(0); }
      fs.appendFileSync(process.env.COMPAT_TRACE, JSON.stringify({pid:process.pid,args})+'\\n');
      // verified: developer Windows log 2026-11-11, conversation 711cb8a1.
      const rejected = ['--hostname','--port','--mdns'].filter(flag => args.some(arg => arg===flag || arg.startsWith(flag+'=')));
      if (process.env.COMPAT_STRICT === '1' && rejected.length) {
        console.log('DESCRIPTION\\n  Start an Agent Client Protocol server\\nUSAGE\\n  opencode acp [flags]');
        console.error('ERRORS');
        for (const flag of rejected) console.error('Unrecognized flag: '+flag+' in command opencode acp');
        process.exit(1);
      }
      const send = value => console.log(JSON.stringify(value));
      require('node:readline').createInterface({input:process.stdin}).on('line', line => {
        const request = JSON.parse(line);
        let result = {};
        if (request.method === 'initialize') result = {protocolVersion:1,agentCapabilities:{loadSession:false,mcpCapabilities:{}},authMethods:[]};
        if (request.method === 'session/new') result = {sessionId:'compat-session'};
        if (request.method === 'session/prompt') {
          send({jsonrpc:'2.0',method:'session/update',params:{sessionId:'compat-session',update:{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'COMPAT_OK'}}}});
          result = {stopReason:'end_turn'};
        }
        if (request.id !== undefined) send({jsonrpc:'2.0',id:request.id,result});
      });
    `);
    const launcher = path.join(temp, process.platform === 'win32' ? 'opencode.cmd' : 'opencode');
    const quote = text => "'" + text.replaceAll("'", "'\\''") + "'";
    await fs.writeFile(launcher, process.platform === 'win32'
      ? `@echo off\r\n"${process.execPath}" "${script}" %*\r\n`
      : `#!/bin/sh\nexec ${quote(process.execPath)} ${quote(script)} "$@"\n`, { mode: 0o755 });
    const listener = net.createServer().listen(0, '127.0.0.1');
    await once(listener, 'listening');
    const { port } = listener.address();
    await new Promise(resolve => listener.close(resolve));
    const base = `http://127.0.0.1:${port}`;
    const home = path.join(temp, 'home');
    await fs.mkdir(home);
    child = spawn(binary, ['--host', '127.0.0.1', '--port', String(port), '--data-dir', path.join(temp, 'data'),
      '--log-dir', path.join(temp, 'logs'), '--work-dir', temp, '--parent-pid', String(process.pid), '--local'], {
      cwd: temp, env: { ...process.env, HOME: home, USERPROFILE: home,
        AIONUI_HANDSHAKE_TIMEOUT_SECS: '8', NO_PROXY: 'localhost,127.0.0.1,::1', no_proxy: 'localhost,127.0.0.1,::1' },
    });
    for (const stream of [child.stdout, child.stderr]) stream.on('data', data => { logs = (logs + data).slice(-12000); });
    const deadline = Date.now() + 45_000;
    while (!(await fetch(base + '/health').catch(() => null))?.ok) {
      assert.equal(child.exitCode, null, logs);
      assert(Date.now() < deadline, 'ACPCore startup timed out: ' + logs);
      await delay(100);
    }
    async function request(route, body, method = body === undefined ? 'GET' : 'POST') {
      const response = await fetch(base + route, { method, signal: AbortSignal.timeout(20_000),
        ...(body === undefined ? {} : { headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }) });
      const value = await response.json();
      assert(response.ok, `${route}: HTTP ${response.status} ${JSON.stringify(value)}`);
      return value.data ?? value;
    }
    const agent = (await request('/api/agents/management')).find(row => row.backend === 'opencode' && row.agent_source === 'builtin');
    assert(agent, 'builtin OpenCode catalog row is required');
    for (const scenario of [
      { name: 'captured-cli-rejects-network-flags', strict: '1', args: [] },
      { name: 'network-capable-cli-keeps-original-argv', strict: '0', args: [] },
      { name: 'explicit-user-arguments-preserved', strict: '0', args: ['--port=61234', '--hostname=127.0.0.1'] },
    ]) {
      await request(`/api/agents/${agent.id}/overrides`, { command_override: launcher, env_override: [
        { name: 'COMPAT_TRACE', value: trace }, { name: 'COMPAT_STRICT', value: scenario.strict },
      ] }, 'PUT');
      await request(`/api/agents/${agent.id}/launch-arguments`, { additional_args: scenario.args }, 'PUT');
      const conversation = await request('/api/conversations', { type: 'acp', name: scenario.name,
        assistant: { id: 'bare:' + agent.id }, extra: { workspace: temp, custom_workspace: true,
          agent_id: agent.id, backend: 'opencode', selected_mcp_server_ids: [], selected_session_mcp_servers: [] } });
      try {
        const ready = await request(`/api/conversations/${conversation.id}/runtime/ensure`, {});
        assert.equal(ready.runtime.can_send_message, true);
        await request(`/api/conversations/${conversation.id}/messages`, { content: 'Synthetic compatibility check' });
        let completed = false;
        for (let attempt = 0; attempt < 100; attempt++) {
          const messages = await request(`/api/conversations/${conversation.id}/messages`);
          const state = await request(`/api/conversations/${conversation.id}`);
          if (JSON.stringify(messages).includes('COMPAT_OK') && !state.runtime.is_processing) { completed = true; break; }
          await delay(50);
        }
        assert(completed, scenario.name + ': expected ACP reply and completed turn');
        const launches = (await fs.readFile(trace, 'utf8')).trim().split('\n').map(JSON.parse);
        assert.deepEqual(launches.at(-1).args, ['acp', ...scenario.args], 'only configured arguments may reach OpenCode');
        report.cases.push({ name: scenario.name, initialized: true, replied: true, argvPreserved: true });
      } finally { await request(`/api/conversations/${conversation.id}`, undefined, 'DELETE'); }
    }
    report.result = 'passed';
    console.log('OpenCode packaged startup compatibility passed: captured flag rejection, capable CLI, user argv and completed ACP turns.');
  } finally {
    if (child && child.exitCode === null) {
      const closed = once(child, 'exit');
      child.kill('SIGTERM');
      await Promise.race([closed, delay(8000)]);
      if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await closed; }
    }
    if (reportPath) {
      await fs.mkdir(path.dirname(reportPath), { recursive: true });
      await fs.writeFile(reportPath, JSON.stringify(report, null, 2) + '\n');
    }
    await fs.rm(temp, { recursive: true, force: true });
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
