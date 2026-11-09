// Exercise the packaged HTTP -> native reader -> serialized catalog writer path.
const assert = require("node:assert/strict");
const { spawn } = require("node:child_process");
const { once } = require("node:events");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { createServer } = require("node:net");
const binary = path.resolve(process.argv[2]);
const fixture = require("../docs/protocol-schemas/claude-model-discovery/initialize.json");
let child;
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

(async () => {
  const temp = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "claude-catalog-")));
  const trace = path.join(temp, "trace.jsonl");
  const responsePath = path.join(temp, "models.json");
  const fakeScript = path.join(temp, "fake-claude.cjs");
  const launcher = path.join(temp, process.platform === "win32" ? "claude.cmd" : "claude");
  try {
    await fs.writeFile(responsePath, JSON.stringify(fixture));
    await fs.writeFile(fakeScript, `
      const fs = require('node:fs');
      if (process.argv.includes('--version')) { console.log('2.1.267 (Claude Code)'); process.exit(0); }
      fs.appendFileSync(process.env.CATALOG_TRACE, JSON.stringify({pid:process.pid,cwd:process.cwd(),args:process.argv.slice(2),kind:'spawn'})+'\\n');
      const readline = require('node:readline').createInterface({input:process.stdin});
      readline.on('line', line => {
        const frame = JSON.parse(line);
        fs.appendFileSync(process.env.CATALOG_TRACE, JSON.stringify({kind:'frame',type:frame.type,subtype:frame.request?.subtype})+'\\n');
        if (frame.request?.subtype === 'initialize') setTimeout(() => {
          const fixture = JSON.parse(fs.readFileSync(process.env.CATALOG_MODELS,'utf8'));
          fixture.response.request_id = frame.request_id;
          console.log(JSON.stringify(fixture));
        }, 150);
      });
    `);
    await fs.writeFile(launcher, process.platform === "win32"
      ? `@echo off\r\n"${process.execPath}" "${fakeScript}" %*\r\n`
      : `#!/bin/sh\nexec '${process.execPath.replaceAll("'", "'\\''")}' '${fakeScript.replaceAll("'", "'\\''")}' "$@"\n`, { mode: 0o755 });
    const socket = createServer().listen(0, "127.0.0.1");
    await once(socket, "listening");
    const { port } = socket.address();
    await new Promise((resolve) => socket.close(resolve));
    const base = `http://127.0.0.1:${port}`;
    child = spawn(binary, ["--host", "127.0.0.1", "--port", String(port), "--data-dir", path.join(temp, "data"),
      "--log-dir", path.join(temp, "logs"), "--work-dir", temp, "--parent-pid", String(process.pid), "--local"], { cwd: temp });
    let logs = "";
    for (const stream of [child.stdout, child.stderr]) stream.on("data", (data) => { logs = (logs + data).slice(-6000); });
    const deadline = Date.now() + 45_000;
    while (Date.now() < deadline && !(await fetch(base + "/health").catch(() => null))?.ok) {
      assert.equal(child.exitCode, null, logs); await delay(100);
    }
    async function request(route, body, method = body === undefined ? "GET" : "POST") {
      const response = await fetch(base + route, { method, signal: AbortSignal.timeout(40_000),
        ...(body === undefined ? {} : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }) });
      const value = await response.json();
      assert(response.ok, `${route}: ${JSON.stringify(value)}`);
      return value.data ?? value;
    }
    const agent = (await request("/api/agents/management")).find((row) => row.backend === "claude" && row.agent_source === "builtin");
    assert(agent);
    await request(`/api/agents/${agent.id}/overrides`, { command_override: launcher, env_override: [
      { name: "CATALOG_TRACE", value: trace }, { name: "CATALOG_MODELS", value: responsePath },
    ] }, "PUT");
    const conversations = await request("/api/conversations");
    const route = `/api/agents/${agent.id}/refresh-config`;
    const [first, second] = await Promise.all([request(route, {}), request(route, {})]);
    const models = (row) => row.config_options.find((option) => option.category === "model").options;
    assert.equal(models(first).length, 6);
    assert.deepEqual(models(first), models(second));
    assert.deepEqual(models(first).map((model) => model.value), fixture.response.response.models.map((model) => model.value));
    assert.equal(models(first).find((model) => model.value === "sonnet").description, "Custom Sonnet model (1M context)");
    assert.deepEqual(await request("/api/conversations"), conversations, "discovery must not create a conversation");
    let events = (await fs.readFile(trace, "utf8")).trim().split("\n").map(JSON.parse);
    assert.equal(events.filter((event) => event.kind === "spawn").length, 1, "concurrent refreshes share one native process");
    assert(events.filter((event) => event.kind === "frame").every((event) => event.type === "control_request" && event.subtype === "initialize"));
    const launched = events.find((event) => event.kind === "spawn");
    assert.equal(launched.cwd, temp);
    assert(launched.args.includes("--no-session-persistence"));
    assert(launched.args.includes("--strict-mcp-config"));
    for (let attempt = 0; attempt < 50; attempt++) {
      try { process.kill(launched.pid, 0); await delay(100); }
      catch (error) { assert.equal(error.code, "ESRCH"); break; }
      if (attempt === 49) assert.fail("discovery process survived cleanup");
    }
    fixture.response.response.models[1].displayName = "updated-provider-model";
    await fs.writeFile(responsePath, JSON.stringify(fixture));
    assert.equal(models(await request(route, {}))[1].name, "updated-provider-model", "a later visit must refresh the catalog");
    const persisted = (await request("/api/agents/management")).find((row) => row.id === agent.id);
    assert.equal(models(persisted)[1].name, "updated-provider-model");
    console.log("Claude catalog packaged smoke passed: native fixture, aliases, coalescing, persistence, no prompt/conversation, and process cleanup.");
  } finally {
    if (child?.exitCode === null && child.signalCode === null) { child.kill(); await once(child, "exit"); }
    await fs.rm(temp, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
})().catch((error) => { console.error(error); process.exitCode = 1; });
