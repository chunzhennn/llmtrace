// Dependency-free Chromium smoke/performance checks against a local test server.
// Usage: node scripts/browser-performance.mjs http://127.0.0.1:PORT REQUEST_ID
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import assert from 'node:assert/strict';

const [base, requestId] = process.argv.slice(2);
assert(['127.0.0.1', 'localhost'].includes(new URL(base).hostname), 'use a local test server');
const profile = await mkdtemp(join(tmpdir(), 'llmtrace-browser-'));
const browser = spawn(process.env.CHROME_BIN ?? '/usr/bin/google-chrome', [
  '--headless=new', '--no-sandbox', '--disable-gpu', '--disable-dev-shm-usage',
  '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0',
  `--user-data-dir=${profile}`, 'about:blank'
], { stdio: 'ignore' });
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
let socket;
try {
  let port;
  for (let i = 0; i < 200; i++) {
    try { port = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]; break; }
    catch { await pause(50); }
  }
  assert(port, 'Chromium did not start');
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  socket = new WebSocket(targets.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  let nextId = 1;
  const pending = new Map();
  const errors = [];
  const warnings = [];
  const requests = [];
  socket.onmessage = event => {
    const message = JSON.parse(event.data);
    if (message.id) {
      const entry = pending.get(message.id);
      pending.delete(message.id);
      clearTimeout(entry?.timer);
      if (message.error) entry?.reject(new Error(JSON.stringify(message.error)));
      else entry?.resolve(message.result);
    }
    if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text);
    if (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error') {
      errors.push(message.params.args.map(arg => arg.value ?? arg.description).join(' '));
    }
    if (message.method === 'Network.loadingFailed' && message.params.errorText !== 'net::ERR_ABORTED') {
      errors.push(message.params.errorText);
    }
    if (message.method === 'Network.requestWillBeSent') requests.push(message.params.request.url);
    if (message.method === 'Network.responseReceived' && message.params.response.status >= 400) {
      const response = message.params.response;
      const target = new URL(response.url).pathname === '/favicon.ico' ? warnings : errors;
      target.push(`${response.status} ${response.url}`);
    }
  };
  function command(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = nextId++;
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`Chromium command timed out: ${method}`));
      }, 15000);
      pending.set(id, { resolve, reject, timer });
      socket.send(JSON.stringify({ id, method, params }));
    });
  }
  async function evaluate(expression) {
    const result = await command('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    assert(!result.exceptionDetails, JSON.stringify(result.exceptionDetails));
    return result.result.value;
  }
  async function waitFor(expression) {
    const deadline = Date.now() + 30000;
    while (Date.now() < deadline) {
      if (await evaluate(expression)) return;
      await pause(50);
    }
    throw new Error(`Browser timeout: ${expression}`);
  }
  await command('Runtime.enable');
  await command('Network.enable');
  await command('Page.enable');
  await command('Performance.enable');
  const login = await fetch(`${base}/api/auth/login`, {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: 'admin' })
  });
  assert.equal(login.status, 200);
  const [name, ...value] = login.headers.get('set-cookie').split(';')[0].split('=');
  await command('Network.setCookie', { name, value: value.join('='), url: base, httpOnly: true });
  const pages = [];
  for (const [path, text] of [
    ['/ui/', 'Overview'], ['/ui/requests', 'Requests'], ['/ui/sessions', 'Sessions'],
    ['/ui/analytics', 'Analytics'], ['/ui/admin/system', 'Archive size rotation']
  ]) {
    const start = performance.now();
    await command('Page.navigate', { url: base + path });
    await waitFor(`document.body.innerText.includes(${JSON.stringify(text)})`);
    if (path === '/ui/admin/system') {
      await waitFor("document.body.textContent.includes('Awaiting disk sync') && document.body.textContent.includes('Pending persistence / cleanup')");
    }
    await pause(150);
    pages.push({ path, ready_ms: performance.now() - start, text_chars: await evaluate('document.body.innerText.length') });
  }
  const startIndex = requests.length;
  await command('Page.navigate', { url: `${base}/ui/requests/${requestId}` });
  await waitFor("document.body.innerText.includes('Request detail') && document.querySelector('[role=tab]') !== null");
  await pause(250);
  assert(!requests.slice(startIndex).some(url => url.includes('include_bodies=true')), 'overview fetched large payloads eagerly');
  const payload = [];
  for (const tab of ['Request', 'Raw']) {
    const start = performance.now();
    await evaluate(`Array.from(document.querySelectorAll('[role=tab]')).find(el => el.textContent.trim() === ${JSON.stringify(tab)}).click()`);
    await waitFor("document.querySelector('pre')?.textContent.length > 1000");
    await pause(100);
    const metrics = Object.fromEntries((await command('Performance.getMetrics')).metrics.map(m => [m.name, m.value]));
    payload.push({ tab, ready_ms: performance.now() - start, rendered_chars: await evaluate('document.querySelector("pre").textContent.length'), heap_bytes: metrics.JSHeapUsedSize, layout_secs: metrics.LayoutDuration, script_secs: metrics.ScriptDuration });
  }
  const payloadRequests = requests.slice(startIndex).filter(url => url.includes('include_bodies=true'));
  assert.equal(payloadRequests.length, 1, 'payload tabs should share one fetch');
  assert(payload.every(item => item.rendered_chars <= 128 * 1024), 'large payload view exceeded its rendering budget');
  // Stub clipboard writes inside this temporary tab so this check cannot
  // overwrite the developer's system clipboard.
  await evaluate("Object.defineProperty(navigator.clipboard, 'writeText', {value: async text => { window.__copiedLength = text.length; }}); document.querySelector('pre').parentElement.querySelector('button').click()");
  await waitFor('window.__copiedLength > 8 * 1024 * 1024');
  const copiedChars = await evaluate('window.__copiedLength');
  assert.deepEqual(errors, [], 'browser console/network failures');
  process.stdout.write(JSON.stringify({ pages, payload, payload_fetches: payloadRequests.length, copied_chars: copiedChars, errors, warnings }) + '\n');
} finally {
  socket?.close();
  browser.kill('SIGKILL');
  await new Promise(resolve => { if (browser.exitCode !== null) resolve(); else browser.once('exit', resolve); });
  await rm(profile, { recursive: true, force: true });
}
