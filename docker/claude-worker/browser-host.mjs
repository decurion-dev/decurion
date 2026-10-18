// Decurion's browser box: the agent's test browser, in a box of its own.
//
// The runner starts this box next to the agent's box, from the same image,
// and talks to it over stdin and stdout, one JSON object per line (the
// protocol is crates/decurion-proto/src/browser.rs). Saved sign-ins only ever
// arrive on stdin, so they are never in a file, a variable or an argument,
// and the agent's box never holds them: the agent only gets the browser
// tools, served on 127.0.0.1:8931, which its box reaches because it shares
// this box's network. Nothing else listens here: Chromium is driven over a
// pipe, with no remote-debugging port.
//
// Two modes:
// - run: the agent's browser for one run, with the app's saved sign-ins.
//   The owner can watch (a screencast) and take over (their clicks and
//   typing); while they have it, the agent's browser calls wait.
// - sign_in: a fresh browser the owner signs in with. No agent, no tools.
//   "Capture" hands back what the browser holds so the runner can save it.
//
// stdout carries protocol lines only. Everything else goes to stderr.

import { createRequire } from 'node:module';
import crypto from 'node:crypto';
import fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';

// Keep stdout for protocol lines: anything that logs goes to stderr.
for (const k of ['log', 'info', 'debug']) console[k] = (...a) => console.error(...a);
const log = (...a) => console.error('[browser-host]', ...a);

// The pinned Playwright MCP package installed globally in the image. Its
// own copy of Playwright is the one the browser was installed with. (The
// variable is only for trying this file outside the image.)
const MCP_PACKAGE =
  process.env.DECURION_PLAYWRIGHT_MCP ?? '/usr/local/lib/node_modules/@playwright/mcp/package.json';
const requireMcp = createRequire(MCP_PACKAGE);

// Limits shared with the runner (crates/decurion-proto/src/browser.rs).
const MAX_FRAME_BYTES = 512 << 10;
const MAX_INPUT_BYTES = 4 << 10;
const MAX_STATE_BYTES = 2 << 20;
// The longest line the runner sends: a start line with every saved sign-in.
const MAX_IN_LINE = 40 << 20;

const MCP_HOST = '127.0.0.1';
const MCP_PORT = 8931;
const MCP_URL = `http://${MCP_HOST}:${MCP_PORT}/mcp`;
// Host headers the tools answer to, so a web page cannot reach them by
// pointing a name of its own at this address.
const MCP_HOSTS = [`${MCP_HOST}:${MCP_PORT}`, `localhost:${MCP_PORT}`];
const MAX_MCP_SESSIONS = 8;

// How long an agent's browser call waits while the owner has the browser.
const OWNER_HOLD_MS = 2 * 60 * 1000;
const OWNER_BUSY = 'The owner is using the browser; wait or do something else.';
// A frame the runner never acknowledged is given up on after this, so a lost
// acknowledgement cannot freeze the screen.
const ACK_TIMEOUT_MS = 10 * 1000;
// Checking a saved sign-in: how long the check page may take, and how soon
// the same sign-in is checked again after a check found it still working.
const PROBE_MS = 30 * 1000;
const RECHECK_MS = 60 * 1000;
const VIEWPORT = { width: 1280, height: 800 };

// The browser tools the agent gets: Playwright MCP's own set, minus every
// tool that reaches past the page into the browser itself. Anything not on
// this list is hidden and refused, including tools a newer version adds.
// Left out on purpose:
// - browser_run_code_unsafe: runs Playwright code with the whole browser in
//   reach, saved sign-ins included (context.storageState(), cookies).
// - browser_network_request: shows each request's headers, and so the
//   Cookie header with sign-in cookies the page itself cannot read.
// - browser_close: there is one shared browser for the run.
// The storage, cookie, network-control and code tools only come with extra
// capabilities, which are never turned on here.
const ALLOWED_TOOLS = new Set([
  'browser_click',
  'browser_console_messages',
  'browser_drag',
  'browser_drop',
  'browser_emulate_media',
  'browser_evaluate',
  'browser_file_upload',
  'browser_fill_form',
  'browser_find',
  'browser_handle_dialog',
  'browser_hover',
  'browser_navigate',
  'browser_navigate_back',
  'browser_network_requests',
  'browser_press_key',
  'browser_resize',
  'browser_select_option',
  'browser_snapshot',
  'browser_tabs',
  'browser_take_screenshot',
  'browser_type',
  'browser_wait_for',
]);

// Playwright MCP's settings, the same as the old in-box browser's flags:
// no tools a web page adds itself (--no-webmcp), its files in /tmp, and
// only the core tools. Files are named by their full path so they can be
// found again and put into the answer (see inlineFiles).
const OUT_DIR = '/tmp/playwright-mcp';
const MCP_CONFIG = {
  browser: { browserName: 'chromium', isolated: false },
  webmcp: false,
  sharedBrowserContext: true,
  outputDir: OUT_DIR,
  filePaths: 'absolute',
  capabilities: [],
};
// The most of one saved file put into an answer.
const MAX_INLINE = 256 << 10;
const TEXT_FILES = new Set(['.yml', '.yaml', '.log', '.txt', '.md', '.json']);

// The named keys the owner's screen can send, as Chrome wants them.
const KEYS = {
  Enter: { key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, text: '\r' },
  Tab: { key: 'Tab', code: 'Tab', windowsVirtualKeyCode: 9 },
  Backspace: { key: 'Backspace', code: 'Backspace', windowsVirtualKeyCode: 8 },
  Escape: { key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 },
  ArrowUp: { key: 'ArrowUp', code: 'ArrowUp', windowsVirtualKeyCode: 38 },
  ArrowDown: { key: 'ArrowDown', code: 'ArrowDown', windowsVirtualKeyCode: 40 },
  ArrowLeft: { key: 'ArrowLeft', code: 'ArrowLeft', windowsVirtualKeyCode: 37 },
  ArrowRight: { key: 'ArrowRight', code: 'ArrowRight', windowsVirtualKeyCode: 39 },
};

// ---- Lines to the runner -------------------------------------------------

function send(msg) {
  process.stdout.write(JSON.stringify(msg) + '\n');
}

let ending = false;
async function fail(why) {
  if (ending) return;
  ending = true;
  send({ type: 'failed', why });
  await shutdown(1);
}

// ---- State of the box ----------------------------------------------------

let mode = null; // 'run' | 'sign_in', once started
let browser = null;
let context = null; // the one the agent (or the owner signing in) uses
let mcpServer = null;
let states = []; // saved sign-ins, as given: { sign_in_id, state, check_url, login_url }
let controller = 'agent';
const ownerWaiters = new Set();
let loginUrl = null; // sign-in mode: where a password was typed

let active = null; // the page shown and driven
const sessions = new WeakMap(); // page -> its CDP session
const cast = { on: false, maxWidth: 1280, quality: 60 };
let castPage = null; // the page the screencast runs on
let inFlight = null; // the frame sent and not yet acknowledged
let seq = 0;
let lastMeta = null; // the latest frame's page size, for input positions
let lastPage = { url: '', title: '' };

// ---- Starting ------------------------------------------------------------

function webAddress(u) {
  if (typeof u !== 'string' || /[\s\u0000-\u001f\u007f]/.test(u)) return false;
  try {
    const p = new URL(u).protocol;
    return p === 'http:' || p === 'https:';
  } catch {
    return false;
  }
}

function validState(s) {
  if (!s || typeof s !== 'object' || !Number.isSafeInteger(s.sign_in_id)) return false;
  const st = s.state;
  if (!st || typeof st !== 'object' || Array.isArray(st)) return false;
  if (st.cookies !== undefined && !Array.isArray(st.cookies)) return false;
  if (st.origins !== undefined && !Array.isArray(st.origins)) return false;
  return JSON.stringify(st).length <= MAX_STATE_BYTES;
}

// One storage state from several: each site's cookies and each origin's
// localStorage. A later sign-in wins where two set the same cookie or key.
function mergeStates(list) {
  const cookies = new Map();
  const origins = new Map();
  for (const st of list) {
    for (const c of st.cookies ?? []) cookies.set(`${c.name}\n${c.domain}\n${c.path}`, c);
    for (const o of st.origins ?? []) {
      if (!o || typeof o.origin !== 'string') continue;
      const items = origins.get(o.origin) ?? new Map();
      for (const kv of o.localStorage ?? []) items.set(kv.name, kv.value);
      origins.set(o.origin, items);
    }
  }
  return {
    cookies: [...cookies.values()],
    origins: [...origins].map(([origin, items]) => ({
      origin,
      localStorage: [...items].map(([name, value]) => ({ name, value })),
    })),
  };
}

async function launch() {
  const { chromium } = requireMcp('playwright-core');
  // Over a pipe (Playwright's default): no debugging port to reach.
  // Chromium's own sandbox cannot start in a box with no capabilities; the
  // box is the sandbox.
  browser = await chromium.launch({ headless: true, chromiumSandbox: false });
  browser.on('disconnected', () => {
    if (!ending) void fail('The browser stopped.');
  });
}

async function startRun(given) {
  states = [];
  for (const s of given ?? []) {
    if (!validState(s)) {
      log(`saved sign-in ${s?.sign_in_id} is not a browser storage state; left out`);
      continue;
    }
    states.push({
      sign_in_id: s.sign_in_id,
      state: s.state,
      check_url: webAddress(s.check_url) ? s.check_url : null,
      login_url: webAddress(s.login_url) ? s.login_url : null,
      reported: false,
      checking: false,
      lastCheck: 0,
      timer: null,
    });
  }
  await launch();
  // Each saved sign-in is checked on its own, in a fresh browser loaded
  // from the saved copy, before the agent starts.
  await Promise.all(states.map((s) => checkSignIn(s)));
  try {
    context = await browser.newContext({
      viewport: VIEWPORT,
      storageState: mergeStates(states.map((s) => s.state)),
    });
  } catch {
    // Not the error itself: it could quote a cookie.
    return fail('The saved sign-ins could not be loaded into the browser.');
  }
  watchContext();
  await context.newPage();
  await serveTools();
  send({ type: 'ready', mcp_url: MCP_URL });
}

async function startSignIn(startUrl) {
  if (!webAddress(startUrl)) return fail('The sign-in page must be a web address.');
  controller = 'owner';
  await launch();
  context = await browser.newContext({ viewport: VIEWPORT });
  watchContext();
  const page = await context.newPage();
  page.goto(startUrl, { waitUntil: 'commit', timeout: PROBE_MS }).catch((e) => log('open failed:', e.message));
}

// ---- Saved sign-ins: are they still good? --------------------------------

function onLoginPage(url, login) {
  try {
    const u = new URL(url);
    const l = new URL(login);
    if (u.origin !== l.origin) return false;
    const path = l.pathname.replace(/\/+$/, '');
    if (path === '') return u.pathname === '/' || u.pathname === '';
    return u.pathname === path || u.pathname.startsWith(path + '/');
  } catch {
    return false;
  }
}

// Opens the sign-in's check page in a fresh browser holding only the saved
// copy: never the agent's browser, which the agent could have signed out.
// true: signed out (sent to the login page, or shown a password field);
// false: still signed in; null: could not tell (the site did not answer).
async function probe(s) {
  if (!s.check_url) return null;
  let ctx = null;
  try {
    ctx = await browser.newContext({ viewport: VIEWPORT, storageState: s.state });
    const page = await ctx.newPage();
    await page.goto(s.check_url, { waitUntil: 'domcontentloaded', timeout: PROBE_MS });
    await page.waitForLoadState('load', { timeout: 10_000 }).catch(() => {});
    // Give a script that sends people to the login page a moment.
    await page.waitForTimeout(1_000);
    if (s.login_url && onLoginPage(page.url(), s.login_url)) return true;
    const passwords = await page.locator('input[type="password"]').filter({ visible: true }).count();
    return passwords > 0;
  } catch (e) {
    log(`could not check saved sign-in ${s.sign_in_id}:`, e?.message?.split('\n')[0]);
    return null;
  } finally {
    await ctx?.close().catch(() => {});
  }
}

async function checkSignIn(s, again = false) {
  if (s.reported || s.checking) return;
  s.checking = true;
  try {
    if ((await probe(s)) === true) {
      s.reported = true;
      send({ type: 'signed_out', sign_in_id: s.sign_in_id });
    }
  } finally {
    s.checking = false;
    if (again) s.lastCheck = Date.now();
  }
}

// The agent's browser landed on a site's login page. That alone proves
// nothing (the agent may have signed out, or followed a link), so the saved
// copy is checked again in a fresh browser before anyone is told. An agent
// that keeps visiting the login page gets one check a minute at most.
function onNavigated(url) {
  if (mode !== 'run') return;
  for (const s of states) {
    if (!s.login_url || s.reported || s.checking || s.timer) continue;
    if (!onLoginPage(url, s.login_url)) continue;
    const wait = s.lastCheck + RECHECK_MS - Date.now();
    if (wait <= 0) {
      void checkSignIn(s, true);
    } else {
      s.timer = setTimeout(() => {
        s.timer = null;
        void checkSignIn(s, true);
      }, wait);
      s.timer.unref();
    }
  }
}

// ---- Pages: which one is shown -------------------------------------------

function watchContext() {
  context.on('page', watchPage);
  for (const p of context.pages()) watchPage(p);
  // Titles change without a navigation; check now and then.
  setInterval(() => void reportPage(), 2_000).unref();
}

function watchPage(page) {
  page.on('framenavigated', (frame) => {
    if (frame !== page.mainFrame()) return;
    onNavigated(frame.url());
    if (page === active) void reportPage();
  });
  page.on('load', () => {
    if (page === active) void reportPage();
  });
  page.on('close', () => {
    if (page !== active) return;
    const rest = context.pages().filter((p) => !p.isClosed());
    void setActive(rest.at(-1) ?? null);
  });
  // The newest page is the one shown, as it is the one the agent opened.
  void setActive(page);
}

async function setActive(page) {
  if (active === page) return;
  active = page;
  lastPage = { url: '', title: '' };
  if (cast.on) await restartCast();
  await reportPage();
}

let reporting = false;
let reportAgain = false;
async function reportPage() {
  // One at a time: a busy page can be slow to give its title.
  if (reporting) {
    reportAgain = true;
    return;
  }
  const page = active;
  if (!page || page.isClosed()) return;
  reporting = true;
  try {
    const url = page.url();
    const slow = new Promise((resolve) => setTimeout(() => resolve(lastPage.title), 3_000).unref());
    const title = await Promise.race([page.title().catch(() => ''), slow]);
    if (page !== active || (url === lastPage.url && title === lastPage.title)) return;
    lastPage = { url, title };
    send({ type: 'page', url, title });
  } finally {
    reporting = false;
    if (reportAgain) {
      reportAgain = false;
      void reportPage();
    }
  }
}

async function cdpFor(page) {
  let s = sessions.get(page);
  if (!s) {
    s = await context.newCDPSession(page);
    s.on('Page.screencastFrame', (f) => onFrame(page, s, f));
    sessions.set(page, s);
  }
  return s;
}

// ---- Screencast ----------------------------------------------------------

async function stopCast() {
  const page = castPage;
  castPage = null;
  inFlight && clearTimeout(inFlight.timer);
  inFlight = null;
  if (page && !page.isClosed()) {
    const s = sessions.get(page);
    await s?.send('Page.stopScreencast').catch(() => {});
  }
}

async function restartCast() {
  await stopCast();
  const page = active;
  if (!cast.on || !page || page.isClosed()) return;
  try {
    const s = await cdpFor(page);
    castPage = page;
    await s.send('Page.startScreencast', {
      format: 'jpeg',
      quality: cast.quality,
      maxWidth: cast.maxWidth,
      maxHeight: cast.maxWidth * 2,
      everyNthFrame: 1,
    });
  } catch (e) {
    log('screencast failed:', e.message);
  }
}

// Width and height of a JPEG, from its frame header.
function jpegSize(b64) {
  const buf = Buffer.from(b64.slice(0, 65536), 'base64');
  let i = 2;
  while (i + 9 < buf.length) {
    if (buf[i] !== 0xff) return null;
    const m = buf[i + 1];
    if (m >= 0xc0 && m <= 0xcf && m !== 0xc4 && m !== 0xc8 && m !== 0xcc) {
      return { height: buf.readUInt16BE(i + 5), width: buf.readUInt16BE(i + 7) };
    }
    i += 2 + buf.readUInt16BE(i + 2);
  }
  return null;
}

// One frame at a time: Chrome sends the next only once this one is
// acknowledged, and that waits for the runner's acknowledgement, so a slow
// viewer gets fewer frames instead of a queue.
function onFrame(page, session, { data, metadata, sessionId }) {
  const ackChrome = () => session.send('Page.screencastFrameAck', { sessionId }).catch(() => {});
  if (!cast.on || page !== castPage || inFlight) return void ackChrome();
  if (data.length > MAX_FRAME_BYTES) {
    // Too heavy to send: lower the quality, then the width, and go again.
    void ackChrome();
    if (cast.quality > 30) cast.quality = Math.max(30, cast.quality - 15);
    else cast.maxWidth = Math.max(320, Math.floor(cast.maxWidth * 0.75));
    void restartCast();
    return;
  }
  const size = jpegSize(data) ?? {
    width: Math.round(metadata.deviceWidth),
    height: Math.round(metadata.deviceHeight),
  };
  lastMeta = metadata;
  seq += 1;
  const mine = seq;
  inFlight = {
    seq: mine,
    ack: ackChrome,
    timer: setTimeout(() => frameAcked(mine), ACK_TIMEOUT_MS),
  };
  send({ type: 'frame', seq: mine, jpeg: data, width: size.width, height: size.height });
}

function frameAcked(n) {
  if (!inFlight || inFlight.seq !== n) return;
  clearTimeout(inFlight.timer);
  const { ack } = inFlight;
  inFlight = null;
  void ack();
}

// ---- The owner's input ---------------------------------------------------

function pageSize(page) {
  if (lastMeta && castPage === page) return { width: lastMeta.deviceWidth, height: lastMeta.deviceHeight };
  return page.viewportSize() ?? VIEWPORT;
}

const frac = (v) => typeof v === 'number' && Number.isFinite(v) && v >= 0 && v <= 1;
const far = (v) => typeof v === 'number' && Number.isFinite(v) && Math.abs(v) <= 10;

async function onInput(input) {
  if (controller !== 'owner') return log('input while the agent has the browser; ignored');
  let page = active && !active.isClosed() ? active : null;
  if (!page) {
    if (input?.kind !== 'go') return;
    page = await context.newPage();
  }
  const s = await cdpFor(page);
  const { width, height } = pageSize(page);
  switch (input?.kind) {
    case 'click': {
      if (!frac(input.x) || !frac(input.y)) return;
      const at = { x: input.x * width, y: input.y * height };
      await s.send('Input.dispatchMouseEvent', { type: 'mouseMoved', ...at });
      await s.send('Input.dispatchMouseEvent', { type: 'mousePressed', button: 'left', clickCount: 1, ...at });
      await s.send('Input.dispatchMouseEvent', { type: 'mouseReleased', button: 'left', clickCount: 1, ...at });
      return;
    }
    case 'scroll': {
      if (!frac(input.x) || !frac(input.y) || !far(input.dx) || !far(input.dy)) return;
      await s.send('Input.dispatchMouseEvent', {
        type: 'mouseWheel',
        x: input.x * width,
        y: input.y * height,
        deltaX: input.dx * width,
        deltaY: input.dy * height,
      });
      return;
    }
    case 'text': {
      if (typeof input.text !== 'string' || !input.text || Buffer.byteLength(input.text) > MAX_INPUT_BYTES) return;
      if (mode === 'sign_in') await notePassword(page);
      await s.send('Input.insertText', { text: input.text });
      return;
    }
    case 'key': {
      const k = KEYS[input.key];
      if (!k) return;
      const { text, ...rest } = k;
      await s.send('Input.dispatchKeyEvent', { type: text ? 'keyDown' : 'rawKeyDown', ...rest, ...(text ? { text, unmodifiedText: text } : {}) });
      await s.send('Input.dispatchKeyEvent', { type: 'keyUp', ...rest });
      return;
    }
    case 'go': {
      if (!webAddress(input.url) || input.url.length > MAX_INPUT_BYTES) return;
      page.goto(input.url, { waitUntil: 'commit', timeout: PROBE_MS }).catch((e) => log('go failed:', e.message.split('\n')[0]));
      return;
    }
    case 'back':
      page.goBack({ waitUntil: 'commit', timeout: PROBE_MS }).catch(() => {});
      return;
  }
}

// Sign-in mode: typing into a password field marks this page as the site's
// login page, which later runs watch for. Asked in Playwright's own world,
// so the page cannot fake the answer by changing its scripts.
async function notePassword(page) {
  try {
    if ((await page.locator('input[type="password"]:focus').count()) > 0) {
      const u = new URL(page.url());
      loginUrl = u.origin + u.pathname;
    }
  } catch {
    // Not knowing the login page only means it is not watched for.
  }
}

// ---- Who drives ----------------------------------------------------------

function setController(who) {
  if (mode !== 'run' || (who !== 'agent' && who !== 'owner')) return;
  controller = who;
  if (who === 'agent') {
    for (const wake of ownerWaiters) wake();
    ownerWaiters.clear();
  }
}

// Resolves true when the agent may use the browser: now, or once the owner
// hands it back. false if the owner keeps it for two minutes, or the call
// was cancelled.
function agentTurn(signal) {
  if (controller === 'agent') return Promise.resolve(true);
  return new Promise((resolve) => {
    const done = (ok) => {
      clearTimeout(timer);
      ownerWaiters.delete(wake);
      signal?.removeEventListener?.('abort', cancel);
      resolve(ok);
    };
    const wake = () => done(true);
    const cancel = () => done(false);
    const timer = setTimeout(() => done(controller === 'agent'), OWNER_HOLD_MS);
    ownerWaiters.add(wake);
    signal?.addEventListener?.('abort', cancel, { once: true });
  });
}

// ---- Sign-in mode: saving ------------------------------------------------

async function capture() {
  if (mode !== 'sign_in' || !context) return;
  let state;
  try {
    state = await context.storageState();
  } catch (e) {
    return fail('The browser could not hand over the sign-in.');
  }
  if (JSON.stringify(state).length > MAX_STATE_BYTES) {
    return fail('This sign-in holds too much to save (over 2 MB). Try again from the site\'s own login page.');
  }
  const cookies = state.cookies ?? [];
  const domains = [...new Set(cookies.map((c) => String(c.domain).replace(/^\./, '')).filter(Boolean))].sort();
  // Session cookies (expires -1) end with the browser and say nothing.
  const lasting = cookies.map((c) => c.expires).filter((e) => typeof e === 'number' && e > 0);
  const expiresAt = lasting.length ? Math.floor(Math.min(...lasting)) : null;
  send({ type: 'captured', state, login_url: loginUrl, domains, expires_at: expiresAt });
}

// ---- The agent's browser tools -------------------------------------------

function toolError(text) {
  return { content: [{ type: 'text', text }], isError: true };
}

// Playwright MCP saves page snapshots, console logs and the like as files
// and answers with a link. The agent cannot open files in this box, so the
// links are swapped for what the files say. Only files under OUT_DIR,
// which only Playwright MCP writes, are read.
async function readOut(file, from, to) {
  const full = path.resolve(file);
  if (!full.startsWith(OUT_DIR + '/')) return null;
  try {
    const st = await fs.stat(full);
    if (!st.isFile() || st.size > MAX_INLINE) return null;
    const text = await fs.readFile(full, 'utf8');
    if (!from) return text;
    return text.split('\n').slice(from - 1, to ?? from).join('\n');
  } catch {
    return null;
  }
}

function fenced(text, lang = '') {
  const longest = Math.max(2, ...[...text.matchAll(/`+/g)].map((m) => m[0].length));
  const fence = '`'.repeat(longest + 1);
  return `${fence}${lang}\n${text}\n${fence}`;
}

async function replaceAsync(text, re, fn) {
  let out = '';
  let last = 0;
  for (const m of text.matchAll(re)) {
    out += text.slice(last, m.index) + (await fn(...m));
    last = m.index + m[0].length;
  }
  return out + text.slice(last);
}

const FILE_LINK = /^- \[([^\]\n]*)\]\(([^)\n]+)\)$/gm;
const LOG_LINK = /^- New console entries: (\S+?)(?:#L(\d+)(?:-L(\d+))?)?$/gm;

async function inlineFiles(result) {
  const hasImage = (result?.content ?? []).some((p) => p?.type === 'image');
  for (const part of result?.content ?? []) {
    if (part?.type !== 'text' || typeof part.text !== 'string' || !part.text.includes(OUT_DIR)) continue;
    let text = await replaceAsync(part.text, FILE_LINK, async (whole, title, file) => {
      if (!file.startsWith(OUT_DIR + '/')) return whole;
      const ext = path.extname(file).toLowerCase();
      if (TEXT_FILES.has(ext)) {
        const body = await readOut(file);
        if (body !== null) return `- ${title}:\n${fenced(body, ext === '.yml' ? 'yaml' : '')}`;
      }
      if (hasImage && ['.png', '.jpg', '.jpeg'].includes(ext)) return `- ${title}: attached as an image`;
      return `- ${title}: too big to show here`;
    });
    text = await replaceAsync(text, LOG_LINK, async (whole, file, from, to) => {
      const body = await readOut(file, from && Number(from), to && Number(to));
      return body === null ? whole : `- New console entries:\n${fenced(body)}`;
    });
    part.text = text;
  }
  return result;
}

// Narrows a Playwright MCP server to the allowed tools, and makes each call
// wait while the owner has the browser. The server keeps its handlers in a
// map by method; the pinned version is checked at image build time
// (`--self-check`), and an unexpected shape stops the box rather than serve
// tools unchecked.
function guard(server) {
  const handlers = server?._requestHandlers;
  if (!(handlers instanceof Map) || !handlers.has('tools/list') || !handlers.has('tools/call')) {
    throw new Error('@playwright/mcp server does not look as expected; refusing to serve it');
  }
  const list = handlers.get('tools/list');
  const call = handlers.get('tools/call');
  handlers.set('tools/list', async (req, extra) => {
    const r = await list(req, extra);
    return { ...r, tools: (r.tools ?? []).filter((t) => ALLOWED_TOOLS.has(t.name)) };
  });
  handlers.set('tools/call', async (req, extra) => {
    const name = req?.params?.name;
    if (!ALLOWED_TOOLS.has(name)) return toolError(`Tool "${name}" is not available in this browser.`);
    if (!(await agentTurn(extra?.signal))) return toolError(OWNER_BUSY);
    // A file name would put the result in this box, out of the agent's
    // reach: leave it out, so the result comes back in the answer.
    const args = req?.params?.arguments;
    let note = null;
    if (args && typeof args === 'object' && 'filename' in args && name !== 'browser_file_upload') {
      note = `Note: the browser runs in a separate box, so nothing was saved as ${JSON.stringify(String(args.filename))}; the result is here instead.`;
      const { filename, ...rest } = args;
      req = { ...req, params: { ...req.params, arguments: rest } };
    }
    const result = await inlineFiles(await call(req, extra));
    if (note) result.content = [{ type: 'text', text: note }, ...(result.content ?? [])];
    return result;
  });
  return server;
}

async function newToolServer() {
  const { createConnection } = requireMcp('./index.js');
  return guard(await createConnection(MCP_CONFIG, async () => context));
}

async function serveTools() {
  const { StreamableHTTPServerTransport } = requireMcp('playwright-core/lib/utilsBundle');
  const live = new Map(); // MCP session id -> transport
  mcpServer = http.createServer(async (req, res) => {
    try {
      // Web pages in this browser share its network too. A browser always
      // says where a request comes from (Origin); the agent's tools never
      // do, so a request that does is a page trying to drive the browser.
      if (!MCP_HOSTS.includes(String(req.headers.host ?? '').toLowerCase()) || req.headers.origin !== undefined) {
        res.statusCode = 403;
        return res.end('Forbidden');
      }
      if (new URL(req.url ?? '/', 'http://localhost').pathname !== '/mcp') {
        res.statusCode = 404;
        return res.end('Not found');
      }
      const id = req.headers['mcp-session-id'];
      if (id) {
        const t = live.get(id);
        if (!t) {
          res.statusCode = 404;
          return res.end('Session not found');
        }
        return await t.handleRequest(req, res);
      }
      if (req.method !== 'POST' || live.size >= MAX_MCP_SESSIONS) {
        res.statusCode = 400;
        return res.end('Invalid request');
      }
      const server = await newToolServer();
      const transport = new StreamableHTTPServerTransport({
        sessionIdGenerator: () => crypto.randomUUID(),
        onsessioninitialized: (sid) => live.set(sid, transport),
      });
      transport.onclose = () => {
        if (transport.sessionId) live.delete(transport.sessionId);
      };
      await server.connect(transport);
      await transport.handleRequest(req, res);
    } catch (e) {
      log('tool request failed:', e?.message);
      if (!res.headersSent) res.statusCode = 500;
      res.end();
    }
  });
  await new Promise((resolve, reject) => {
    mcpServer.once('error', reject);
    mcpServer.listen(MCP_PORT, MCP_HOST, resolve);
  });
}

// ---- The runner's lines --------------------------------------------------

async function onLine(line) {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return log('unreadable line from the runner; ignored');
  }
  if (msg?.type === 'start') {
    if (mode) return log('second start; ignored');
    mode = msg.mode === 'sign_in' ? 'sign_in' : msg.mode === 'run' ? 'run' : null;
    if (!mode) return fail('Unknown browser mode.');
    try {
      return await (mode === 'run' ? startRun(msg.states) : startSignIn(msg.start_url));
    } catch (e) {
      log('start failed:', e?.stack ?? e);
      return fail(`The browser could not start: ${String(e?.message ?? e).split('\n')[0]}`);
    }
  }
  if (!context) return;
  switch (msg?.type) {
    case 'screencast':
      cast.on = msg.on === true;
      if (Number.isInteger(msg.max_width)) cast.maxWidth = Math.min(1920, Math.max(320, msg.max_width));
      if (Number.isInteger(msg.quality)) cast.quality = Math.min(90, Math.max(20, msg.quality));
      if (cast.on) {
        lastPage = { url: '', title: '' };
        await reportPage();
      }
      return cast.on ? restartCast() : stopCast();
    case 'frame_ack':
      return frameAcked(msg.seq);
    case 'input':
      return onInput(msg.input);
    case 'control':
      return setController(msg.who);
    case 'capture':
      return capture();
    default:
      return log(`unknown line type ${JSON.stringify(String(msg?.type)).slice(0, 40)}; ignored`);
  }
}

// Lines in order, each handled before the next, at most MAX_IN_LINE long.
function readLines() {
  let buf = '';
  let skipping = false;
  let chain = Promise.resolve();
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk) => {
    let start = 0;
    for (let nl = chunk.indexOf('\n'); nl !== -1; nl = chunk.indexOf('\n', start)) {
      const piece = chunk.slice(start, nl);
      start = nl + 1;
      if (!skipping) {
        const line = buf + piece;
        chain = chain.then(() => onLine(line)).catch((e) => log('line failed:', e?.message));
      }
      buf = '';
      skipping = false;
    }
    if (!skipping) buf += chunk.slice(start);
    if (buf.length > MAX_IN_LINE) {
      log('line from the runner too long; skipped');
      buf = '';
      skipping = true;
    }
  });
  // The runner closed stdin: the run is over.
  process.stdin.on('end', () => void chain.then(() => shutdown(0)));
}

async function shutdown(code) {
  ending = true;
  mcpServer?.close();
  await browser?.close().catch(() => {});
  process.exit(code);
}

// Checks the pinned Playwright MCP has the shape this file relies on, and
// prints the tools the agent will get. Run when the image is built.
async function selfCheck() {
  const server = await newToolServer();
  const r = await server._requestHandlers.get('tools/list')({ method: 'tools/list', params: {} }, {});
  const names = r.tools.map((t) => t.name);
  const missing = [...ALLOWED_TOOLS].filter((n) => !names.includes(n));
  if (missing.length) throw new Error(`allowed tools missing from @playwright/mcp: ${missing.join(', ')}`);
  const refused = await server._requestHandlers.get('tools/call')(
    { method: 'tools/call', params: { name: 'browser_run_code_unsafe', arguments: {} } },
    {},
  );
  if (!refused.isError) throw new Error('browser_run_code_unsafe was not refused');
  const { StreamableHTTPServerTransport } = requireMcp('playwright-core/lib/utilsBundle');
  if (typeof StreamableHTTPServerTransport !== 'function') throw new Error('no HTTP transport');
  log(`ok: ${names.length} tools: ${names.join(' ')}`);
}

process.on('unhandledRejection', (e) => log('unhandled:', e?.message ?? e));
process.on('uncaughtException', (e) => {
  log('crashed:', e?.stack ?? e);
  void fail('The browser box stopped on an error.');
});
process.on('SIGTERM', () => void shutdown(0));

if (process.argv.includes('--self-check')) {
  selfCheck().then(
    () => process.exit(0),
    (e) => {
      log('self-check failed:', e?.message);
      process.exit(1);
    },
  );
} else {
  readLines();
}
