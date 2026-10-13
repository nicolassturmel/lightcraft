/* The only browser state is connection, input and the latest preview. No WASM engine,
   catalogue, RAW decoder, model, original download, IndexedDB or OPFS is initialized. */
'use strict';
const canvas = document.getElementById('view');
const context = canvas.getContext('2d', { alpha: false });
const notice = document.getElementById('notice');
const status = document.getElementById('status');
const stage = document.getElementById('stage');
let viewport = null;
let frameState = null;
let connected = false;
let connecting = false;
let fetching = false;
let sending = false;
let nextFrame = 0;
let lastActivity = 0;
let connection = 0;
let input = [];
let held = new Set();
let heldKeys = new Set();
let lastPoint = { x: .5, y: .5 };
let actions = [];
const key = new URLSearchParams(location.hash.slice(1)).get('key');
if (key) history.replaceState(null, '', location.pathname);
function show(message, offline = false) {
  document.getElementById('message').textContent = message;
  notice.hidden = !message;
  if (offline) { connected = false; viewport = null; input = []; held.clear(); heldKeys.clear(); status.textContent = 'Déconnecté'; }
}
async function fetchBounded(path, options = {}) {
  const response = await fetch(path, { cache: 'no-store', credentials: 'same-origin', ...options, signal: AbortSignal.timeout(15000) });
  if (response.status === 401) { const error = new Error('Autorisation requise. Ouvre cette vue sur le Mac ou utilise son lien de connexion.'); error.auth = true; throw error; }
  if (!response.ok) { const error = new Error('Le Mac ne répond pas encore.'); error.busy = response.status === 503; throw error; }
  return response;
}
async function rpc(method, params = {}) {
  const response = await fetchBounded('/api/rpc', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ method, params }) });
  const data = await response.json();
  if (!data.ok) throw new Error(data.error || 'Cette action n’a pas pu être appliquée.');
  return data.result;
}
async function connect() {
  if (connecting) return;
  connecting = true;
  connection++;
  show('Connexion à LightCraft sur le Mac…');
  try {
    await fetchBounded('/api/connect', key ? { headers: { Authorization: 'Bearer ' + key } } : {});
    connected = true;
    show('');
    status.textContent = 'Vue du Mac';
    nextFrame = 0;
    await updateFrame();
  } catch (error) { show(error.auth ? error.message : 'Connexion interrompue. Vérifie que LightCraft est ouvert sur le Mac.', true); }
  finally { connecting = false; }
}
function fit() {
  if (!viewport) return;
  const scale = Math.min(stage.clientWidth / viewport[0], stage.clientHeight / viewport[1]);
  canvas.style.width = (viewport[0] * scale) + 'px';
  canvas.style.height = (viewport[1] * scale) + 'px';
}
async function updateFrame() {
  if (!connected || fetching || document.hidden) return;
  fetching = true;
  const generation = connection;
  try {
    const response = await fetchBounded('/api/frame');
    const width = Number(response.headers.get('X-LightCraft-Width'));
    const height = Number(response.headers.get('X-LightCraft-Height'));
    if (![width, height].every(v => Number.isFinite(v) && v >= 1 && v <= 32768)) throw new Error('Aperçu invalide.');
    const size = Number(response.headers.get('Content-Length'));
    if (!Number.isFinite(size) || size > 16 * 1024 * 1024) throw new Error('Aperçu trop grand.');
    const blob = await response.blob();
    if (blob.size > 16 * 1024 * 1024) throw new Error('Aperçu trop grand.');
    const bitmap = await createImageBitmap(blob);
    if (generation !== connection) { bitmap.close(); return; }
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    context.drawImage(bitmap, 0, 0);
    bitmap.close();
    viewport = [width, height];
    frameState = response.headers.get('X-LightCraft-State');
    canvas.style.cursor = response.headers.get('X-LightCraft-Cursor') || 'default';
    fit();
    show('');
    status.textContent = 'Vue du Mac';
  } catch (error) {
    if (!error.busy) { show(error.auth ? error.message : 'Connexion interrompue. Vérifie que LightCraft est ouvert sur le Mac.', true); }
  } finally {
    fetching = false;
    nextFrame = performance.now() + (performance.now() - lastActivity < 1800 ? 70 : 500);
  }
}
function modifiers(event) { return { alt: event.altKey, ctrl: event.ctrlKey, shift: event.shiftKey, os: event.metaKey, cmd: event.metaKey || event.ctrlKey }; }
function point(event) {
  const rect = canvas.getBoundingClientRect();
  return { x: Math.max(0, Math.min(1, (event.clientX - rect.left) / Math.max(1, rect.width))), y: Math.max(0, Math.min(1, (event.clientY - rect.top) / Math.max(1, rect.height))) };
}
function queue(event) {
  if (!connected || !viewport) return;
  lastActivity = performance.now();
  // Coalesce motion only; preserve each press/release and key in order.
  if (event.kind === 'move' && input.at(-1)?.kind === 'move') input[input.length - 1] = event;
  else if (input.length < 256) input.push(event);
  else { show('Le Mac traite encore les actions. Relâche le pointeur puis réessaie.'); return; }
  flush();
}
async function flush() {
  if (sending || !connected || !viewport || !input.length) return;
  sending = true;
  const events = input.splice(0, 64);
  try { await rpc('ui.webInput', { viewport, state: frameState, events }); nextFrame = 0; }
  catch (error) { input = []; held.clear(); show(error.message); nextFrame = 0; }
  finally { sending = false; if (input.length) setTimeout(flush, 20); }
}
canvas.addEventListener('focus', () => queue({ kind: 'focus', focused: true }));
canvas.addEventListener('blur', () => { release(); queue({ kind: 'focus', focused: false }); });
canvas.addEventListener('pointerdown', event => {
  if (!viewport) return;
  event.preventDefault(); canvas.focus({ preventScroll: true }); queue({ kind: 'focus', focused: true });
  canvas.setPointerCapture(event.pointerId); held.add(event.button); lastPoint = point(event);
  queue({ kind: 'down', ...lastPoint, button: event.button, ...modifiers(event) });
});
canvas.addEventListener('pointermove', event => { lastPoint = point(event); queue({ kind: 'move', ...lastPoint, ...modifiers(event) }); });
canvas.addEventListener('pointerup', event => {
  event.preventDefault(); lastPoint = point(event); held.delete(event.button);
  queue({ kind: 'up', ...lastPoint, button: event.button, ...modifiers(event) });
  if (!held.size && canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
});
function release() { for (const button of held) queue({ kind: 'up', ...lastPoint, button }); held.clear(); for (const name of heldKeys) queue({ kind: 'key', key: name, pressed: false }); heldKeys.clear(); queue({ kind: 'gone' }); }
canvas.addEventListener('pointercancel', release);
canvas.addEventListener('lostpointercapture', () => { if (held.size) release(); });
canvas.addEventListener('pointerleave', () => { if (!held.size) queue({ kind: 'gone' }); });
canvas.addEventListener('contextmenu', event => event.preventDefault());
canvas.addEventListener('wheel', event => {
  event.preventDefault(); const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? 600 : 1;
  queue({ kind: 'scroll', ...point(event), dx: Math.max(-4096, Math.min(4096, -event.deltaX * unit)), dy: Math.max(-4096, Math.min(4096, -event.deltaY * unit)), ...modifiers(event) });
}, { passive: false });
canvas.addEventListener('keydown', event => {
  if (event.isComposing || event.key === 'Process') return;
  let name = /^[a-zA-Z0-9]$/.test(event.key) ? event.key.toUpperCase() : event.key;
  const mapping = { ' ': 'Space', ArrowLeft: 'Left', ArrowRight: 'Right', ArrowUp: 'Up', ArrowDown: 'Down' };
  name = mapping[name] || name;
  const mods = modifiers(event);
  if (mods.cmd && ['L','R','W','T','N','Q'].includes(name)) return;
  const text = event.key.length === 1 && !mods.cmd && !mods.ctrl ? event.key : '';
  if (/^[A-Z0-9]$/.test(name) || ['Space','Enter','Escape','Tab','Backspace','Delete','Left','Right','Up','Down','Home','End','PageUp','PageDown','+','-','=','[',']','/','\\','.'].includes(name)) {
    if (!(mods.cmd && name === 'V')) event.preventDefault();
    heldKeys.add(name);
    queue({ kind: 'key', key: name, pressed: true, text, repeat: event.repeat, ...mods });
  } else if (text) { event.preventDefault(); queue({ kind: 'text', text }); }
});
canvas.addEventListener('keyup', event => {
  let name = /^[a-zA-Z0-9]$/.test(event.key) ? event.key.toUpperCase() : event.key;
  name = ({ ' ': 'Space', ArrowLeft: 'Left', ArrowRight: 'Right', ArrowUp: 'Up', ArrowDown: 'Down' })[name] || name;
  if (heldKeys.delete(name)) { event.preventDefault(); queue({ kind: 'key', key: name, pressed: false, ...modifiers(event) }); }
});
canvas.addEventListener('paste', event => { event.preventDefault(); const text = event.clipboardData?.getData('text/plain'); if (text) queue({ kind: 'paste', text: text.slice(0, 16000) }); });
canvas.addEventListener('compositionend', event => { if (event.data) queue({ kind: 'text', text: event.data.slice(0, 16000) }); });
window.addEventListener('blur', () => { release(); queue({ kind: 'focus', focused: false }); });
window.addEventListener('focus', () => { if (document.activeElement === canvas) queue({ kind: 'focus', focused: true }); });
window.addEventListener('resize', fit);
document.getElementById('retry').addEventListener('click', connect);
document.getElementById('fullscreen').addEventListener('click', async () => { try { if (document.fullscreenElement) await document.exitFullscreen(); else await document.documentElement.requestFullscreen(); } catch { show('Le plein écran n’est pas disponible dans ce navigateur.'); } });
document.getElementById('share').addEventListener('click', async () => {
  try {
    const info = await (await fetchBounded('/api/info')).json();
    if (!info.shareUrl) throw new Error('Lien indisponible.');
    try { await navigator.clipboard.writeText(info.shareUrl); status.textContent = 'Lien copié'; }
    catch { const field = document.getElementById('share-link'); field.value = info.shareUrl; field.hidden = false; field.focus(); field.select(); }
  } catch (error) { show(error.message); }
});
function listCommands() {
  const container = document.getElementById('command-list'); container.replaceChildren();
  const term = document.getElementById('search').value.toLocaleLowerCase();
  const groups = new Map();
  for (const action of actions) {
    if (term && !(action.label + ' ' + action.id).toLocaleLowerCase().includes(term)) continue;
    const group = action.menu?.[0] || 'Actions';
    if (!groups.has(group)) groups.set(group, []); groups.get(group).push(action);
  }
  for (const [group, commands] of groups) {
    const heading = document.createElement('h3'); heading.textContent = group; container.append(heading);
    for (const action of commands) {
      const button = document.createElement('button'); button.textContent = action.label;
      const shortcut = document.createElement('small'); shortcut.textContent = action.shortcut || ''; button.append(shortcut); button.disabled = !action.enabled;
      button.addEventListener('click', async () => {
        document.getElementById('commands').hidden = true;
        try { await rpc('engine.execute', { command: action.id, params: {} }); lastActivity = performance.now(); nextFrame = 0; }
        catch (error) { show(error.message); }
      }); container.append(button);
    }
  }
}
document.getElementById('actions').addEventListener('click', async () => {
  try { actions = await rpc('engine.commands'); document.getElementById('commands').hidden = false; listCommands(); document.getElementById('search').focus(); }
  catch (error) { show(error.message); }
});
document.getElementById('close-actions').addEventListener('click', () => { document.getElementById('commands').hidden = true; canvas.focus(); });
document.getElementById('search').addEventListener('input', listCommands);
setInterval(() => { if (connected && performance.now() >= nextFrame) updateFrame(); }, 35);
setInterval(() => { if (held.size || heldKeys.size) queue({ kind: 'heartbeat' }); }, 500);
document.addEventListener('visibilitychange', () => { if (document.hidden) release(); else { nextFrame = 0; if (!connected) connect(); } });
connect();
