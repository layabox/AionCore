/** Exercise the pinned Pi adapter through ACPCore without sending a model prompt. */
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const { existsSync } = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const http = require('node:http');
const net = require('node:net');
const { spawn, execFileSync } = require('node:child_process');
const { once } = require('node:events');
const crypto = require('node:crypto');
const binary = path.resolve(process.argv[2]);
const reportPath = process.argv[3] && path.resolve(process.argv[3]);
const lock = require('../crates/aionui-runtime/resources/acp-registry-npx-lock.json').agents.pi;
let child, server, root;
let logs = '';
async function stop() {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill();
    await Promise.race([once(child, 'exit'), new Promise((_, reject) => setTimeout(() => reject(new Error('isolated ACPCore did not stop')), 12000))]);
  }
  child = null;
}
async function freePort() {
  const probe = net.createServer().listen(0, '127.0.0.1');
  await once(probe, 'listening');
  const port = probe.address().port;
  await new Promise(resolve => probe.close(resolve));
  return port;
}
async function start() {
  const base = `http://127.0.0.1:${await freePort()}`;
  child = spawn(binary, ['--host', '127.0.0.1', '--port', new URL(base).port,
    '--data-dir', path.join(root, 'data'), '--work-dir', root, '--log-dir', path.join(root, 'logs'),
    '--parent-pid', String(process.pid), '--local', '--managed-resources-mode', 'download'], { cwd: root });
  for (const stream of [child.stdout, child.stderr]) stream.on('data', data => { logs = (logs + data).slice(-12000); });
  const deadline = Date.now() + 45000;
  while (Date.now() < deadline) {
    if ((await fetch(base + '/health').catch(() => null))?.ok) {
      return async (route, body) => {
        const response = await fetch(base + route, { signal: AbortSignal.timeout(45000),
          ...(body === undefined ? {} : { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }) });
        const value = await response.json();
        assert(response.ok, `${route}: ${JSON.stringify(value)}`);
        return value.data ?? value;
      };
    }
    if (child.exitCode !== null || child.signalCode !== null) throw new Error(logs);
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw new Error('isolated ACPCore startup timeout: ' + logs);
}
function result(request) {
  if (request.method === 'initialize') return { protocolVersion: request.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'pi-audit', version: '1.0.0' } };
  if (request.method === 'tools/list') return { tools: [{ name: 'audit_echo', description: 'Isolated MCP echo', inputSchema: { type: 'object', properties: {} } }] };
  if (request.method === 'tools/call') return { content: [{ type: 'text', text: 'PI_MCP_OK' }] };
  return {};
}
(async () => {
  root = await fs.mkdtemp(path.join(os.tmpdir(), 'acpcore-pi-mcp-'));
  const npmCandidates = [path.join(path.dirname(process.execPath), 'node_modules/npm/bin/npm-cli.js'), path.resolve(path.dirname(process.execPath), '../lib/node_modules/npm/bin/npm-cli.js')];
  const npm = npmCandidates.find(existsSync);
  assert(npm, 'npm CLI must be installed next to the CI Node runtime');
  const npmArgs = ['--cache', path.join(root, 'npm-cache')];
  const packed = JSON.parse(execFileSync(process.execPath, [npm, 'pack', `${lock.package}@${lock.version}`, '--ignore-scripts', '--json', ...npmArgs], { cwd: root, encoding: 'utf8' }));
  const archive = path.join(root, packed[0].filename);
  const bytes = await fs.readFile(archive);
  assert.equal('sha512-' + crypto.createHash('sha512').update(bytes).digest('base64'), lock.integrity);
  const prefix = path.join(root, 'dependencies');
  execFileSync(process.execPath, [npm, 'install', '--prefix', prefix, '--ignore-scripts', '--no-audit', '--no-fund', archive, '@earendil-works/pi-coding-agent@1.5.1', ...npmArgs], { cwd: root, stdio: 'inherit' });
  const adapter = path.join(prefix, 'node_modules/@jhillyerd/pi-acp/dist/index.js');
  const pi = path.join(prefix, 'node_modules/.bin', process.platform === 'win32' ? 'pi.cmd' : 'pi');
  assert(existsSync(adapter) && existsSync(pi));
  const httpCalls = [];
  const literal = '!printf LITERAL_${ACP_AUDIT_UNSET}';
  const receivedHeaders = [];
  server = http.createServer(async (req, res) => {
    if (req.method !== 'POST') { res.writeHead(405).end(); return; }
    let body = ''; for await (const chunk of req) body += chunk;
    const request = JSON.parse(body); httpCalls.push(request.method); receivedHeaders.push(req.headers['x-audit-literal']);
    if (request.id === undefined) { res.writeHead(202).end(); return; }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: result(request) }));
  }).listen(0, '127.0.0.1');
  await once(server, 'listening');
  const url = `http://127.0.0.1:${server.address().port}/mcp`;
  const marker = path.join(root, 'stdio-calls.ndjson');
  const stdio = path.join(root, 'mcp.cjs');
  await fs.writeFile(stdio, `const fs=require('node:fs');const rl=require('node:readline').createInterface({input:process.stdin});const result=${result.toString()};rl.on('line',line=>{const r=JSON.parse(line);fs.appendFileSync(process.argv[2],JSON.stringify({method:r.method,literal:process.env.AUDIT_LITERAL})+'\\n');if(r.id!==undefined)process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result:result(r)})+'\\n');});`);
  let request = await start(); await stop();
  const env = [
    { name: 'PI_CODING_AGENT_DIR', value: path.join(root, 'pi-agent') },
    { name: 'PI_ACP_PI_COMMAND', value: pi },
    { name: 'OPENAI_API_KEY', value: 'isolated-audit-placeholder-no-model-prompts' },
  ];
  const seed = `import sqlite3,json,sys\np=json.load(sys.stdin)\nc=sqlite3.connect(p['db'])\nc.execute("INSERT INTO agent_metadata (id,agent_id,name,backend,agent_type,agent_source,agent_source_info,enabled,command,args,env,behavior_policy,sort_order,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",('pi-mcp-audit','pi-mcp-audit','Pi MCP audit','pi','acp','custom','{}',1,p['node'],json.dumps([p['adapter']]),json.dumps(p['env']),'{}',9999,1,1))\nc.commit()`;
  execFileSync(process.platform === 'win32' ? 'python' : 'python3', ['-c', seed], { input: JSON.stringify({ db: path.join(root, 'data/aionui-backend.db'), node: process.execPath, adapter, env }) });
  request = await start();
  const conversation = await request('/api/conversations', { type: 'acp', name: 'Pi MCP isolated audit', extra: {
    workspace: root, custom_workspace: true, agent_id: 'pi-mcp-audit', backend: 'pi', selected_mcp_server_ids: [],
    selected_session_mcp_servers: [
      { id: 'audit-http', name: 'audit-http', transport: { type: 'http', url, headers: { 'x-audit-literal': literal } } },
      { id: 'audit-stdio', name: 'audit-stdio', transport: { type: 'stdio', command: process.execPath, args: [stdio, marker], env: { AUDIT_LITERAL: literal } } },
      { id: 'audit-sse', name: 'audit-sse', transport: { type: 'sse', url, headers: {} } },
    ] } });
  await request(`/api/conversations/${conversation.id}/runtime/ensure`, {});
  const deadline = Date.now() + 25000;
  let stdioCalls = [];
  while (Date.now() < deadline) {
    stdioCalls = (await fs.readFile(marker, 'utf8').catch(() => '')).split('\n').filter(Boolean).map(line => JSON.parse(line));
    if (httpCalls.includes('tools/list') && stdioCalls.some(call => call.method === 'tools/list')) break;
    await new Promise(resolve => setTimeout(resolve, 150));
  }
  assert(httpCalls.includes('initialize') && httpCalls.includes('tools/list'), 'Pi did not discover the HTTP MCP tools');
  assert(stdioCalls.some(call => call.method === 'initialize') && stdioCalls.some(call => call.method === 'tools/list'), 'Pi did not discover the stdio MCP tools');
  assert(receivedHeaders.every(value => value === literal), 'HTTP header must remain literal');
  assert(stdioCalls.every(call => call.literal === literal), 'MCP child env must remain literal');
  const snapshot = await request(`/api/conversations/${conversation.id}`);
  const statuses = Object.fromEntries(snapshot.extra.mcp_statuses.map(status => [status.name, status.status]));
  assert.equal(statuses['audit-http'], 'loaded'); assert.equal(statuses['audit-stdio'], 'loaded'); assert.equal(statuses['audit-sse'], 'unsupported');
  if (reportPath) {
    await fs.mkdir(path.dirname(reportPath), { recursive: true });
    await fs.writeFile(reportPath, JSON.stringify({ package: lock.package, version: lock.version, sourceCommit: lock.source_commit, integrity: lock.integrity, archiveSha256: crypto.createHash('sha256').update(bytes).digest('hex'), nativePi: '1.5.1', verified: ['ACPCore session injection', 'stdio initialize and tools/list', 'HTTP initialize and tools/list', 'SSE remains unsupported'], modelPrompts: 0, toolCalls: 0 }, null, 2));
  }
  console.log('Pi 1.5.1: ACP-provided stdio and HTTP tools discovered through the pinned adapter; SSE rejected. No model prompt sent.');
})().catch(error => { console.error(error); process.exitCode = 1; }).finally(async () => {
  await stop();
  server?.closeAllConnections();
  if (server) await new Promise(resolve => server.close(resolve));
  if (root) await fs.rm(root, { recursive: true, force: true, maxRetries: 3 });
});
