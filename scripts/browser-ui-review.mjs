// Review only the isolated synthetic fixture described in docs/ui-review.md.
// node scripts/browser-ui-review.mjs FIXTURE_JSON OUTPUT_DIR [BASE_URL]
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, writeFile, mkdir, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import assert from 'node:assert/strict';
const [fixturePath, output, override] = process.argv.slice(2);
const fixture = JSON.parse(await readFile(fixturePath, 'utf8'));
assert.equal(fixture.synthetic, true);
const base = override ?? fixture.base;
assert(['127.0.0.1', 'localhost'].includes(new URL(base).hostname));
await mkdir(output, { recursive: true });
const profile = await mkdtemp(join(tmpdir(), 'llmtrace-ui-browser-'));
const browser = spawn(process.env.CHROME_BIN ?? '/usr/bin/google-chrome', [
  '--headless=new', '--no-sandbox', '--disable-gpu', '--disable-dev-shm-usage',
  '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0',
  `--user-data-dir=${profile}`, 'about:blank'
], { stdio: 'ignore' });
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
let socket;
let captureFailure;
const report = { synthetic: true, screenshots: [], errors: [], checks: {}, timing: [] };
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
  const requests = new Set();
  const requestUrls = [];
  socket.onmessage = event => {
    const message = JSON.parse(event.data);
    if (message.id) {
      const entry = pending.get(message.id);
      pending.delete(message.id);
      clearTimeout(entry?.timer);
      if (message.error) entry?.reject(new Error(JSON.stringify(message.error)));
      else entry?.resolve(message.result);
    }
    if (message.method === 'Runtime.exceptionThrown') report.errors.push(message.params.exceptionDetails.text);
    if (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error') report.errors.push(message.params.args.map(arg => arg.value ?? arg.description).join(' '));
    if (message.method === 'Network.requestWillBeSent' && message.params.request.url.includes('/api/')) { requests.add(message.params.requestId); requestUrls.push(message.params.request.url); }
    if (['Network.loadingFinished', 'Network.loadingFailed'].includes(message.method)) requests.delete(message.params.requestId);
  };
  function command(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = nextId++;
      const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Chromium timeout: ${method}`)); }, 15000);
      pending.set(id, { resolve, reject, timer });
      socket.send(JSON.stringify({ id, method, params }));
    });
  }
  captureFailure = async () => {
    const {data}=await command('Page.captureScreenshot',{format:'png'});
    await writeFile(join(output,'failure.png'),Buffer.from(data,'base64'));
    await writeFile(join(output,'report.json'),JSON.stringify(report,null,2)+'\n');
  };
  async function evaluate(expression) {
    const result = await command('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    assert(!result.exceptionDetails, JSON.stringify(result.exceptionDetails));
    return result.result.value;
  }
  async function waitFor(expression) {
    const deadline = Date.now() + 30000;
    while (Date.now() < deadline) { if (await evaluate(expression)) return; await pause(50); }
    throw new Error(`Browser timeout: ${expression}`);
  }
  async function settle() {
    const start=performance.now();
    await pause(200);
    for (let i = 0; requests.size && i < 200; i++) await pause(50);
    await pause(350);
    report.timing.push({wait_ms:Math.round(performance.now()-start),pending:requests.size});
  }
  async function click(selector) {
    const point = await evaluate(`(() => { const e=document.querySelector(${JSON.stringify(selector)}); if (!e) throw Error('Missing element'); e.scrollIntoView({block:'center'}); const r=e.getBoundingClientRect(); return {x:r.x+r.width/2,y:r.y+r.height/2}; })()`);
    await command('Input.dispatchMouseEvent', { type:'mousePressed', button:'left', clickCount:1, ...point });
    await command('Input.dispatchMouseEvent', { type:'mouseReleased', button:'left', clickCount:1, ...point });
  }
  async function clickText(selector, text) {
    await evaluate(`(() => { const e=Array.from(document.querySelectorAll(${JSON.stringify(selector)})).find(e => e.textContent.trim() === ${JSON.stringify(text)}); if(!e) throw Error('Missing text target: '+${JSON.stringify(text)}); e.setAttribute('data-review-target','true'); })()`);
    await click('[data-review-target]');
    await evaluate("document.querySelector('[data-review-target]')?.removeAttribute('data-review-target')");
  }
  async function key(key, code = key, modifiers = 0) {
    await command('Input.dispatchKeyEvent', { type:'keyDown', key, code, modifiers, windowsVirtualKeyCode:{Tab:9,Enter:13,Escape:27,ArrowRight:39,ArrowLeft:37,Home:36,End:35}[key] });
    await command('Input.dispatchKeyEvent', { type:'keyUp', key, code, modifiers });
  }
  async function navigate(path, title) {
    requests.clear();
    await command('Page.navigate', {url:base+path});
    await waitFor(`document.querySelector('h1')?.textContent.includes(${JSON.stringify(title)})`);
    await settle();
  }
  async function shot(name) {
    await settle();
    await evaluate('window.scrollTo(0,0)');
    const {data} = await command('Page.captureScreenshot', {format:'png',captureBeyondViewport:false});
    await writeFile(join(output, name+'.png'), Buffer.from(data,'base64'));
    process.stdout.write(`Captured ${name} (pending API: ${requests.size}, settle: ${report.timing.at(-1)?.wait_ms} ms)\n`);
    report.screenshots.push({name, ...await evaluate(`({width:innerWidth,height:innerHeight,documentWidth:document.documentElement.scrollWidth,clientWidth:document.documentElement.clientWidth,title:document.title,text:document.body.innerText.slice(0,6000)})`)});
  }
  for (const method of ['Runtime.enable','Network.enable','Page.enable']) await command(method);
  report.browser = (await command('Browser.getVersion')).product;
  await command('Emulation.setDeviceMetricsOverride', {width:1440,height:1000,deviceScaleFactor:1,mobile:false});
  await navigate('/ui/login', 'llmtrace');
  await shot('desktop-login');
  report.checks.disabledSsoHidden = await evaluate("!document.body.textContent.includes('Continue with SSO')");
  await click('#username'); await command('Input.insertText',{text:'admin'});
  await click('#password'); await command('Input.insertText',{text:'admin'});
  await click('button[type=submit]');
  await settle();
  await waitFor("document.querySelector('h1')?.textContent === 'Overview'");
  report.checks.login = true;
  const pages = [
    ['overview','/ui/','Overview'], ['requests','/ui/requests','Requests'],
    ['sessions','/ui/sessions','Sessions'], ['session',`/ui/sessions/${fixture.session_id}`,'Session detail'],
    ['request',`/ui/requests/${fixture.request_id}`,'Request detail'],
    ['analytics','/ui/analytics','Analytics'], ['query','/ui/query','Query'],
    ['system','/ui/admin/system','System'], ['plugins','/ui/admin/plugins','Plugins'],
    ['audit','/ui/audit','Audit'], ['ui-sessions','/ui/admin/ui-sessions','UI Sessions'],
    ['redaction','/ui/admin/redaction','Redaction']
  ];
  for (const [name,path,title] of pages) { await navigate(path,title); await shot('desktop-'+name); }
  await navigate(`/ui/requests/${fixture.request_id}`,'Request detail');
  await click('[role=tab]');
  await key('ArrowRight');
  report.checks.arrowKeyChangesTab = await evaluate("document.querySelector('[role=tab][aria-selected=true]')?.textContent.trim() === 'Request'");
  await navigate('/ui/', 'Overview');
  await click('[aria-label="Toggle theme"]');
  await shot('desktop-overview-light');
  report.checks.themePersists = await evaluate("localStorage.getItem('llmtrace:theme') === 'light'");
  await click('[aria-label="Toggle theme"]');
  await command('Emulation.setDeviceMetricsOverride', {width:390,height:844,deviceScaleFactor:1,mobile:false});
  for (const [name,path,title] of pages.slice(0,7)) { await navigate(path,title); await shot('mobile-'+name); }
  await click('[aria-label="Open menu"]');
  await shot('mobile-menu');
  report.checks.menuFocusInside = await evaluate("document.querySelector('aside').contains(document.activeElement)");
  await key('Tab', 'Tab', 8);
  report.checks.menuFocusTrapped = await evaluate("document.querySelector('aside').contains(document.activeElement)");
  await key('Escape');
  await settle();
  report.checks.escapeClosesMenu = await evaluate("document.querySelector('aside').getBoundingClientRect().right <= 0");
  report.checks.menuExpandedAttribute = await evaluate("document.querySelector('[aria-label=\"Open menu\"]').getAttribute('aria-expanded') === 'false'");
  report.checks.closedMenuInert = await evaluate("document.querySelector('aside').inert");
  report.checks.menuRestoresFocus = await evaluate("document.activeElement?.getAttribute('aria-label') === 'Open menu'");
  report.checks.sessionSearchLabel = await (async()=>{await navigate('/ui/sessions','Sessions');return evaluate("!!document.querySelector('input')?.labels?.length || !!document.querySelector('input')?.getAttribute('aria-label')");})();

  // Search, detail navigation and a real page export must agree with the visible rows.
  await command('Emulation.setDeviceMetricsOverride', {width:1440,height:1000,deviceScaleFactor:1,mobile:false});
  await navigate('/ui/sessions','Sessions');
  await click('input[name=q]'); await command('Input.insertText',{text:'Maya'});
  await click('button[type=submit]');
  await waitFor("location.search.includes('q=Maya')"); await settle();
  report.checks.employeeSearch = await evaluate("document.querySelectorAll('tbody tr').length === 2 && Array.from(document.querySelectorAll('tbody tr')).every(e=>e.textContent.includes('Maya Chen'))");
  await click('tbody a'); await waitFor("document.querySelector('h1')?.textContent === 'Session detail'"); await settle();
  await clickText('main a','Back'); await waitFor("document.querySelector('h1')?.textContent === 'Sessions'");
  report.checks.sessionBackPreservesSearch = await evaluate("location.search === '?q=Maya'");
  await navigate('/ui/requests?limit=25','Requests');
  report.checks.filterLabels = await evaluate("Array.from(document.querySelectorAll('main input, main select')).every(e=>e.labels?.length)");
  await click('main input[type=text]'); await command('Input.insertText',{text:'claude'});
  await click('button[type=submit]'); await waitFor("location.search.includes('q=claude')"); await settle();
  report.checks.requestSearch = await evaluate("document.querySelectorAll('tbody tr').length === 25 && Array.from(document.querySelectorAll('tbody tr')).every(e=>e.textContent.includes('claude-sonnet-4'))");
  await click('[aria-label="Next page"]'); await waitFor("location.search.includes('offset=25')"); await settle();
  const expectedIds = await evaluate("Array.from(document.querySelectorAll('tbody tr')).map(row=>new URL(row.querySelector('a').href).pathname.split('/').at(-1))");
  report.checks.pagination = expectedIds.length === 7;
  const downloadDir = join(profile,'downloads'); await mkdir(downloadDir);
  await command('Browser.setDownloadBehavior',{behavior:'allow',downloadPath:downloadDir});
  await clickText('main button','Export page (JSONL)'); await settle();
  let files=[];
  for(let i=0;i<100;i++) { files=(await readdir(downloadDir)).filter(f=>f.endsWith('.jsonl')); if(files.length)break;await pause(50); }
  assert.equal(files.length,1,'export download');
  const exported=(await readFile(join(downloadDir,files[0]),'utf8')).trim().split('\n').map(line=>JSON.parse(line));
  report.checks.exportMatchesCurrentPage = JSON.stringify(exported.map(row=>row.id ?? row.request?.id)) === JSON.stringify(expectedIds);
  await click('tbody a'); await waitFor("document.querySelector('h1')?.textContent === 'Request detail'"); await settle();
  await clickText('main a','Back'); await waitFor("document.querySelector('h1')?.textContent === 'Requests'"); await settle();
  report.checks.requestBackPreservesFilters = await evaluate("location.search.includes('q=claude') && location.search.includes('offset=25')");
  await navigate('/ui/sessions?q=does-not-exist','Sessions');
  report.checks.emptySearchState = await evaluate("document.body.textContent.includes('No sessions matched your search.')");
  await shot('desktop-empty-search');
  const bodyFetchStart=requestUrls.length;
  await navigate(`/ui/requests/${fixture.request_id}`,'Request detail');
  report.checks.payloadIsLazy = !requestUrls.slice(bodyFetchStart).some(url=>url.includes('include_bodies=true'));
  for(const tab of ['Request','Response','Raw']) {await clickText('[role=tab]',tab);await settle();}
  report.checks.payloadFetchShared = requestUrls.slice(bodyFetchStart).filter(url=>url.includes('include_bodies=true')).length === 1;
  await clickText('[role=tab]','Tool calls (1)');await settle();
  report.checks.toolCallVisible = await evaluate("document.body.textContent.includes('search_internal_docs')");
  await shot('desktop-tool-call');
  await navigate('/ui/analytics','Analytics'); await clickText('[role=tab]','Users');await settle();
  report.checks.employeeCostsVisible = await evaluate("document.body.textContent.includes('Maya Chen') && document.body.textContent.toLowerCase().includes('cost')");
  await shot('desktop-employee-analytics');
  await navigate('/ui/query','Query');
  report.checks.querySortReadable = await evaluate("document.querySelector('[aria-label=\"Sort field\"]').getBoundingClientRect().width > 120");
  await clickText('main button','Run query');await settle();
  report.checks.structuredQuery = await evaluate("document.querySelectorAll('tbody tr').length > 0");
  await shot('desktop-query-results');
  await command('Emulation.setDeviceMetricsOverride', {width:390,height:844,deviceScaleFactor:1,mobile:false});
  await navigate(`/ui/sessions/${fixture.session_id}`,'Session detail');await click('a[href="#transcript"]');await settle();
  const {data:transcriptImage}=await command('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});
  await writeFile(join(output,'mobile-transcript.png'),Buffer.from(transcriptImage,'base64'));
  report.checks.transcriptShortcut = await evaluate("document.querySelector('#transcript').getBoundingClientRect().top >= 0 && document.querySelector('#transcript').getBoundingClientRect().top < 160");
  await command('Emulation.setDeviceMetricsOverride', {width:320,height:740,deviceScaleFactor:1,mobile:false});
  for(const [name,path,title] of pages.slice(0,7)){await navigate(path,title);await shot('narrow-'+name);}
  report.checks.noPageOverflow = report.screenshots.every(s=>s.documentWidth <= s.clientWidth);
  await click('[aria-label="Log out"]');await waitFor("location.pathname === '/ui/login' && document.querySelector('#username') !== null");
  report.checks.logout = await evaluate("document.querySelector('#username') !== null");

  await writeFile(join(output,'report.json'), JSON.stringify(report,null,2)+'\n');
  process.stdout.write(JSON.stringify({screenshots:report.screenshots.length,checks:report.checks,errors:report.errors})+'\n');
  if (process.env.LLMTRACE_UI_VERIFY === '1') { assert.deepEqual(report.errors,[]); for(const [name,passed] of Object.entries(report.checks)) assert.equal(passed,true,name); }
} catch (error) {
  await captureFailure?.();
  throw error;
} finally {
  socket?.close(); browser.kill('SIGKILL');
  await new Promise(resolve => {if (browser.exitCode !== null) resolve();else browser.once('exit',resolve);});
  await rm(profile,{recursive:true,force:true,maxRetries:10,retryDelay:100});
}
