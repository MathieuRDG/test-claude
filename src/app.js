'use strict';

// ---------------------------------------------------------------------------
// Pont vers le backend Rust (Tauri)
// ---------------------------------------------------------------------------
const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
const listen = (event, cb) => window.__TAURI__.event.listen(event, cb);

const $ = (id) => document.getElementById(id);
const el = {
  text: $('text'),
  backdrop: $('backdrop'),
  highlights: $('highlights'),
  issues: $('issues'),
  status: $('status'),
  source: $('source'),
  lang: $('lang'),
  notice: $('notice'),
  noticeText: $('notice-text'),
  popover: $('popover'),
  toast: $('toast'),
  counts: $('counts'),
  hint: $('hint'),
  apply: $('btn-apply'),
  fixAll: $('btn-fix-all'),
  copy: $('btn-copy'),
  dialog: $('settings'),
  form: $('settings-form'),
};

const KIND_LABEL = { orthographe: 'Orthographe', grammaire: 'Grammaire', typographie: 'Typographie', style: 'Style' };
const FALLBACK_LANGS = [
  ['auto', 'Détection automatique'], ['fr', 'Français'], ['en-US', 'Anglais (États-Unis)'],
  ['en-GB', 'Anglais (Royaume-Uni)'], ['es', 'Espagnol'], ['de-DE', 'Allemand'],
  ['it', 'Italien'], ['pt-PT', 'Portugais'], ['nl', 'Néerlandais'],
];
const PUBLIC_SERVER = 'https://api.languagetool.org';

let config = null;
let platform = { os: 'linux', wayland: false, shortcut_active: false, version: '', config_path: '' };
let issues = [];
let issueSeq = 0;
let activeKey = null;
let lastText = '';
let checkTimer = null;
let checkSeq = 0;
let checking = false;
let dirty = false;
let lastError = null;
let detectedLang = '';
let fromSelection = false;
let captureMode = 'selection';
let lastCaptureId = 0;
let pendingShortcut = null;
let recording = false;
const ignored = new Set();

const isMac = () => platform.os === 'macos';
const modLabel = () => (isMac() ? '⌘' : 'Ctrl');
const ignoreKey = (i) => `${i.rule_id}\u0000${i.original}`;
const plural = (n, one, many) => `${n} ${n > 1 ? many : one}`;

// ---------------------------------------------------------------------------
// Petits utilitaires DOM
// ---------------------------------------------------------------------------
function h(tag, attrs = {}, children = []) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') node.className = v;
    else if (k === 'text') node.textContent = v;
    else if (k.startsWith('on')) node.addEventListener(k.slice(2), v);
    else node.setAttribute(k, v);
  }
  for (const c of [].concat(children)) if (c != null) node.append(c);
  return node;
}

let toastTimer = null;
function toast(message) {
  el.toast.textContent = message;
  el.toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.toast.hidden = true; }, 2200);
}

function showNotice(message, level = 'info') {
  if (!message) return hideNotice();
  el.noticeText.textContent = message;
  el.notice.className = `notice ${level}`;
  el.notice.hidden = false;
}
function hideNotice() { el.notice.hidden = true; }

// Rend visibles les espaces spéciales dans les suggestions (typographie française).
function displaySuggestion(r) {
  if (r === '') return '(supprimer)';
  if (r.trim() === '') {
    if (r.includes(' ')) return 'espace fine insécable';
    if (r.includes(' ')) return 'espace insécable';
    return r.length > 1 ? 'espaces' : 'espace';
  }
  return r.replace(/[  ]/g, '·');
}

// ---------------------------------------------------------------------------
// Vérification
// ---------------------------------------------------------------------------
function scheduleCheck(delay) {
  clearTimeout(checkTimer);
  checkTimer = setTimeout(runCheck, delay);
}

async function runCheck() {
  clearTimeout(checkTimer);
  const text = el.text.value;
  const seq = ++checkSeq;
  dirty = false;
  if (!text.trim()) {
    issues = []; lastError = null; checking = false; detectedLang = '';
    render();
    return;
  }
  checking = true;
  renderStatus();
  try {
    const res = await invoke('check_text', { text, language: config.language });
    if (seq !== checkSeq || el.text.value !== text) return; // résultat périmé
    lastError = null;
    detectedLang = languageName(res.language_code, res.language);
    issues = res.issues
      .filter((i) => !ignored.has(ignoreKey(i)))
      .map((i) => ({ ...i, key: ++issueSeq }));
    if (!issues.some((i) => i.key === activeKey)) activeKey = null;
  } catch (e) {
    if (seq !== checkSeq) return;
    lastError = String(e);
  } finally {
    if (seq === checkSeq) {
      checking = false;
      render();
    }
  }
}

// Décale les fautes connues quand le texte est modifié, pour que le soulignement
// reste en place sans attendre la prochaine vérification.
function rebase(before, after) {
  if (before === after) return;
  const max = Math.min(before.length, after.length);
  let p = 0;
  while (p < max && before.charCodeAt(p) === after.charCodeAt(p)) p++;
  let s = 0;
  while (s < max - p && before.charCodeAt(before.length - 1 - s) === after.charCodeAt(after.length - 1 - s)) s++;
  const oldEnd = before.length - s;
  const delta = after.length - before.length;
  issues = issues
    .filter((i) => i.offset + i.length <= p || i.offset >= oldEnd)
    .map((i) => (i.offset >= oldEnd ? { ...i, offset: i.offset + delta } : i));
  if (!issues.some((i) => i.key === activeKey)) activeKey = null;
}

const sortedIssues = () => [...issues].sort((a, b) => a.offset - b.offset || b.length - a.length);

// ---------------------------------------------------------------------------
// Rendu
// ---------------------------------------------------------------------------
function render() {
  renderHighlights();
  renderIssues();
  renderStatus();
  renderButtons();
}

function renderHighlights() {
  const text = el.text.value;
  const frag = document.createDocumentFragment();
  let pos = 0;
  for (const i of sortedIssues()) {
    if (i.offset < pos || i.offset + i.length > text.length) continue;
    if (i.offset > pos) frag.append(text.slice(pos, i.offset));
    const mark = h('mark', { class: i.kind + (i.key === activeKey ? ' active' : '') });
    mark.dataset.key = i.key;
    mark.textContent = text.slice(i.offset, i.offset + i.length);
    frag.append(mark);
    pos = i.offset + i.length;
  }
  // L'espace final garantit la même hauteur que la zone de saisie si le texte finit par un retour à la ligne.
  frag.append(text.slice(pos) + ' ');
  el.highlights.replaceChildren(frag);
  syncScroll();
}

function syncScroll() {
  el.backdrop.scrollTop = el.text.scrollTop;
  el.backdrop.scrollLeft = el.text.scrollLeft;
}

function issueCard(i, inPopover = false) {
  const card = h('div', { class: `card ${i.kind}${i.key === activeKey || inPopover ? ' active' : ''}` });
  card.dataset.key = i.key;
  const orig = i.original.trim() ? i.original : '␣';
  card.append(
    h('div', { class: 'card-head' }, [
      h('span', { class: 'kind', text: KIND_LABEL[i.kind] || i.kind }),
      h('span', { class: 'orig', text: orig, title: i.original }),
    ]),
    h('div', { class: 'msg', text: i.message }),
  );
  const sugg = h('div', { class: 'suggestions' });
  if (i.replacements.length) {
    for (const r of i.replacements) {
      sugg.append(h('button', {
        type: 'button', class: 'sugg', text: displaySuggestion(r),
        title: /[  ]/.test(r) ? '· = espace insécable' : `Remplacer par « ${r} »`,
        onclick: (ev) => { ev.stopPropagation(); applyReplacement(i, r); },
      }));
    }
  } else {
    sugg.append(h('span', { class: 'nosugg', text: 'Aucune suggestion' }));
  }
  const actions = h('div', { class: 'actions' }, [
    h('button', { type: 'button', text: 'Ignorer', onclick: (ev) => { ev.stopPropagation(); ignoreIssue(i); } }),
    i.kind === 'orthographe'
      ? h('button', { type: 'button', text: 'Ajouter au dictionnaire', onclick: (ev) => { ev.stopPropagation(); addToDictionary(i); } })
      : null,
    h('button', {
      type: 'button', text: 'Désactiver la règle', title: `${i.rule_id} — ${i.rule_description}`,
      onclick: (ev) => { ev.stopPropagation(); disableRule(i); },
    }),
  ]);
  card.append(sugg, actions);
  if (!inPopover) card.addEventListener('click', () => focusIssue(i));
  return card;
}

function renderIssues() {
  const list = sortedIssues();
  const nodes = [];
  if (lastError) {
    nodes.push(h('div', { class: 'card error' }, [
      h('div', { class: 'card-head' }, h('span', { class: 'kind', text: 'Erreur' })),
      h('div', { class: 'msg', text: lastError }),
      h('div', { class: 'actions' }, [
        h('button', { type: 'button', text: 'Réessayer', onclick: runCheck }),
        h('button', { type: 'button', text: 'Paramètres du serveur', onclick: openSettings }),
      ]),
    ]));
  } else if (!list.length) {
    let msg;
    if (!el.text.value.trim()) msg = 'Les corrections apparaîtront ici.';
    else if (checking || dirty) msg = 'Vérification…';
    else msg = 'Aucune faute détectée.';
    const ok = msg === 'Aucune faute détectée.';
    nodes.push(h('div', { class: 'empty' }, [ok ? h('span', { class: 'big', text: '✓' }) : null, msg]));
  }
  for (const i of list) nodes.push(issueCard(i));
  el.issues.replaceChildren(...nodes);
}

function renderStatus() {
  const s = el.status;
  s.className = 'status';
  if (checking) s.textContent = 'Vérification…';
  else if (lastError) { s.textContent = 'Serveur injoignable'; s.classList.add('fail'); }
  else if (dirty && !config.live) s.textContent = 'Modifié — cliquer pour vérifier';
  else if (!el.text.value.trim()) s.textContent = '';
  else if (issues.length) { s.textContent = plural(issues.length, 'correction suggérée', 'corrections suggérées'); s.classList.add('errors'); }
  else { s.textContent = 'Aucune faute'; s.classList.add('ok'); }

  const t = el.text.value;
  const words = (t.match(/[\p{L}\p{N}’'-]+/gu) || []).length;
  el.counts.textContent = t ? `${plural(words, 'mot', 'mots')} · ${plural(t.length, 'caractère', 'caractères')}${detectedLang ? ` · ${detectedLang}` : ''}` : '';
}

function renderButtons() {
  el.apply.hidden = !fromSelection;
  el.fixAll.disabled = !issues.some((i) => i.replacements.length && i.kind !== 'style');
  el.copy.disabled = !el.text.value;
}

function updateSource() {
  el.source.textContent = !fromSelection ? 'Éditeur' : captureMode === 'field' ? 'Champ capturé' : 'Sélection capturée';
  el.source.title = fromSelection && captureMode === 'field' ? '« Remplacer » remplacera tout le texte du champ' : '';
  el.source.classList.toggle('selection', fromSelection);
  renderButtons();
}

function updateHint() {
  const parts = [];
  if (platform.wayland) parts.push('Raccourci : lie « coquille --corriger » dans ton bureau');
  else if (platform.shortcut_active) parts.push(`Raccourci global : ${config.shortcut}`);
  parts.push(`${modLabel()}+Entrée : ${fromSelection ? 'remplacer' : 'copier'}`);
  parts.push('Échap : fermer');
  if (config.server_url === PUBLIC_SERVER) parts.push('serveur public');
  el.hint.textContent = parts.join(' · ');
}

// ---------------------------------------------------------------------------
// Actions sur les fautes
// ---------------------------------------------------------------------------
function replaceRange(start, end, value) {
  const ta = el.text;
  const scroll = ta.scrollTop;
  ta.focus({ preventScroll: true });
  ta.setSelectionRange(start, end);
  let ok = false;
  try {
    // execCommand conserve l'historique d'annulation (Ctrl+Z).
    ok = value === '' ? document.execCommand('delete') : document.execCommand('insertText', false, value);
  } catch { ok = false; }
  if (!ok || ta.value.slice(start, start + value.length) !== value) {
    ta.setRangeText(value, start, end, 'end');
    ta.dispatchEvent(new Event('input'));
  }
  ta.scrollTop = scroll;
  syncScroll();
}

function applyReplacement(i, value) {
  if (el.text.value.slice(i.offset, i.offset + i.length) !== i.original) { runCheck(); return; }
  hidePopover();
  activeKey = null;
  replaceRange(i.offset, i.offset + i.length, value);
}

function fixAll() {
  const text = el.text.value;
  let out = '';
  let pos = 0;
  let n = 0;
  for (const i of sortedIssues()) {
    if (!i.replacements.length || i.kind === 'style' || i.offset < pos) continue;
    if (text.slice(i.offset, i.offset + i.length) !== i.original) continue;
    out += text.slice(pos, i.offset) + i.replacements[0];
    pos = i.offset + i.length;
    n++;
  }
  if (!n) return;
  out += text.slice(pos);
  hidePopover();
  const caret = el.text.selectionStart;
  replaceRange(0, text.length, out);
  el.text.setSelectionRange(Math.min(caret, out.length), Math.min(caret, out.length));
  toast(`${plural(n, 'correction appliquée', 'corrections appliquées')} — ${modLabel()}+Z pour annuler`);
  if (!config.live) runCheck();
}

function ignoreIssue(i) {
  ignored.add(ignoreKey(i));
  issues = issues.filter((x) => ignoreKey(x) !== ignoreKey(i));
  hidePopover();
  render();
}

async function addToDictionary(i) {
  try {
    config = await invoke('add_to_dictionary', { word: i.original });
    const w = i.original.toLowerCase();
    issues = issues.filter((x) => !(x.kind === 'orthographe' && x.original.toLowerCase() === w));
    hidePopover();
    render();
    toast(`« ${i.original} » ajouté au dictionnaire`);
  } catch (e) { showNotice(String(e), 'error'); }
}

async function disableRule(i) {
  try {
    config = await invoke('disable_rule', { ruleId: i.rule_id });
    issues = issues.filter((x) => x.rule_id !== i.rule_id);
    hidePopover();
    render();
    toast('Règle désactivée (réactivable dans les paramètres)');
  } catch (e) { showNotice(String(e), 'error'); }
}

function markFor(i) {
  return el.highlights.querySelector(`mark[data-key="${i.key}"]`);
}

function focusIssue(i) {
  activeKey = i.key;
  hidePopover();
  render();
  el.text.focus({ preventScroll: true });
  el.text.setSelectionRange(i.offset, i.offset + i.length);
  const mark = markFor(i);
  if (mark) {
    const top = mark.offsetTop - el.text.clientHeight / 3;
    el.text.scrollTop = Math.max(0, top);
    syncScroll();
  }
}

function issueAt(pos) {
  return sortedIssues().find((i) => pos >= i.offset && pos <= i.offset + i.length);
}

function showPopover(i) {
  const mark = markFor(i);
  if (!mark) return;
  el.popover.replaceChildren(issueCard(i, true));
  el.popover.hidden = false;
  const r = mark.getClientRects()[0] || mark.getBoundingClientRect();
  const pw = el.popover.offsetWidth;
  const ph = el.popover.offsetHeight;
  const left = Math.min(Math.max(8, r.left - 12), window.innerWidth - pw - 8);
  let top = r.bottom + 8;
  if (top + ph > window.innerHeight - 8) top = Math.max(8, r.top - ph - 8);
  el.popover.style.left = `${left}px`;
  el.popover.style.top = `${top}px`;
}

function hidePopover() { el.popover.hidden = true; }

// ---------------------------------------------------------------------------
// Capture depuis une autre application, copie, remplacement
// ---------------------------------------------------------------------------
function setText(text) {
  el.text.value = text;
  lastText = text;
  issues = [];
  activeKey = null;
  lastError = null;
  detectedLang = '';
  el.text.scrollTop = 0;
  render();
}

function applyCapture(c) {
  if (!c || c.id <= lastCaptureId) return;
  lastCaptureId = c.id;
  hidePopover();
  if (c.from_selection) {
    fromSelection = true;
    captureMode = c.mode || 'selection';
    setText(c.text);
    hideNotice();
    runCheck();
  } else {
    fromSelection = false;
    showNotice(c.notice, 'warn');
    render();
  }
  updateSource();
  updateHint();
  el.text.focus();
  el.text.setSelectionRange(0, 0);
}

async function copyText() {
  if (!el.text.value) return;
  try {
    await invoke('copy_text', { text: el.text.value });
    toast('Texte copié dans le presse-papiers');
  } catch (e) { showNotice(String(e), 'error'); }
}

async function applyToSource() {
  if (!fromSelection) return copyText();
  try {
    const res = await invoke('apply_text', { text: el.text.value });
    if (res.pasted) {
      fromSelection = false;
      updateSource();
      updateHint();
      hideNotice();
    } else {
      showNotice(res.message, 'info');
    }
  } catch (e) { showNotice(String(e), 'error'); }
}

// ---------------------------------------------------------------------------
// Langues
// ---------------------------------------------------------------------------
let displayNames = null;
try { displayNames = new Intl.DisplayNames(['fr'], { type: 'language' }); } catch { /* navigateur ancien */ }
function languageName(code, fallback = '') {
  let label = fallback || code || '';
  try { if (displayNames && code) label = displayNames.of(code) || label; } catch { /* code inconnu */ }
  return label ? label.charAt(0).toUpperCase() + label.slice(1) : '';
}

function fillLanguages(list) {
  const current = config.language;
  const options = list.map(([code, name]) => h('option', { value: code, text: name }));
  if (!list.some(([code]) => code === current)) options.push(h('option', { value: current, text: current }));
  el.lang.replaceChildren(...options);
  el.lang.value = current;
}

async function loadServerLanguages() {
  try {
    const langs = await invoke('list_languages');
    const seen = new Set();
    const list = [];
    for (const l of langs) {
      if (seen.has(l.longCode)) continue;
      seen.add(l.longCode);
      list.push([l.longCode, languageName(l.longCode, l.name)]);
    }
    list.sort((a, b) => a[1].localeCompare(b[1], 'fr'));
    fillLanguages([['auto', 'Détection automatique'], ...list]);
  } catch { /* on garde la liste par défaut */ }
}

async function changeLanguage() {
  try {
    config = await invoke('save_config', { config: { ...config, language: el.lang.value } });
    runCheck();
  } catch (e) { showNotice(String(e), 'error'); }
}

// ---------------------------------------------------------------------------
// Paramètres
// ---------------------------------------------------------------------------
const lines = (s) => s.split('\n').map((x) => x.trim()).filter(Boolean);

async function openSettings() {
  hidePopover();
  const f = el.form.elements;
  f.server_url.value = config.server_url;
  f.username.value = config.username;
  f.api_key.value = config.api_key;
  pendingShortcut = null;
  recording = false;
  renderShortcut();
  f.live.checked = config.live;
  f.bubble.checked = config.bubble;
  f.bubble_excluded_apps.value = config.bubble_excluded_apps.join('\n');
  $('bubble-settings').hidden = !platform.bubble_supported;
  f.picky.checked = config.picky;
  f.restore_clipboard.checked = config.restore_clipboard;
  f.dictionary.value = config.dictionary.join('\n');
  f.disabled_rules.value = config.disabled_rules.join('\n');
  f.autostart.checked = await invoke('get_autostart').catch(() => false);
  $('shortcut-native').hidden = platform.wayland;
  $('shortcut-wayland').hidden = !platform.wayland;
  $('test-result').textContent = '';
  $('test-result').className = 'small';
  $('settings-error').textContent = '';
  $('about').textContent = `Coquille ${platform.version} · ${platform.config_path}`;
  if (!el.dialog.open) el.dialog.showModal();
}

async function saveSettings(ev) {
  ev.preventDefault();
  const f = el.form.elements;
  const next = {
    ...config,
    server_url: f.server_url.value,
    username: f.username.value,
    api_key: f.api_key.value,
    shortcut: pendingShortcut || config.shortcut,
    live: f.live.checked,
    bubble: f.bubble.checked,
    bubble_excluded_apps: lines(f.bubble_excluded_apps.value),
    picky: f.picky.checked,
    restore_clipboard: f.restore_clipboard.checked,
    dictionary: lines(f.dictionary.value),
    disabled_rules: lines(f.disabled_rules.value),
  };
  const serverChanged = next.server_url.trim() !== config.server_url;
  if (recording) await stopRecording();
  try {
    config = await invoke('save_config', { config: next });
  } catch (e) {
    $('settings-error').textContent = String(e);
    return;
  }
  pendingShortcut = null;
  try { await invoke('set_autostart', { enabled: f.autostart.checked }); } catch (e) {
    toast(`Lancement automatique : ${e}`);
  }
  platform = await invoke('platform_info');
  el.dialog.close();
  updateHint();
  if (serverChanged) loadServerLanguages();
  if (platform.shortcut_error) showNotice(platform.shortcut_error, 'warn');
  else if (!el.notice.classList.contains('info')) hideNotice();
  runCheck();
  toast('Paramètres enregistrés');
}

async function testServer() {
  const out = $('test-result');
  out.className = 'small muted';
  out.textContent = 'Test en cours…';
  try {
    out.textContent = await invoke('test_server', { url: el.form.elements.server_url.value });
    out.className = 'small ok-text';
  } catch (e) {
    out.textContent = String(e);
    out.className = 'small error-text';
  }
}

// Saisie du raccourci : on enregistre la combinaison réellement pressée.
const CODE_KEYS = {
  Space: 'Space', Enter: 'Enter', Tab: 'Tab', Backquote: 'Backquote', Minus: 'Minus', Equal: 'Equal',
  BracketLeft: 'BracketLeft', BracketRight: 'BracketRight', Backslash: 'Backslash', Semicolon: 'Semicolon',
  Quote: 'Quote', Comma: 'Comma', Period: 'Period', Slash: 'Slash', Insert: 'Insert', Delete: 'Delete',
  Home: 'Home', End: 'End', PageUp: 'PageUp', PageDown: 'PageDown',
  ArrowUp: 'Up', ArrowDown: 'Down', ArrowLeft: 'Left', ArrowRight: 'Right',
};
function keyFromEvent(e) {
  // Lettres : Windows et X11 raisonnent sur la touche affichée, macOS sur la position physique.
  if (!isMac() && /^[a-z]$/i.test(e.key)) return e.key.toUpperCase();
  if (/^Key[A-Z]$/.test(e.code)) return e.code.slice(3);
  if (/^Digit[0-9]$/.test(e.code)) return e.code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(e.code)) return e.code;
  return CODE_KEYS[e.code] || null;
}
const prettyShortcut = (acc) => acc.split('+').map((k) => ({ Space: 'Espace', Shift: 'Maj', Super: 'Win', Cmd: '⌘' }[k] || k)).join(' + ');

function renderShortcut() {
  const rec = $('shortcut-rec');
  rec.classList.toggle('recording', recording);
  $('shortcut-keys').textContent = recording ? '…' : prettyShortcut(pendingShortcut || config.shortcut);
  $('shortcut-hint').textContent = recording ? 'Appuie sur la nouvelle combinaison… (Échap : annuler)' : 'Cliquer pour changer';
  const status = $('shortcut-status');
  if (recording) {
    status.className = 'small muted';
    status.textContent = 'Combinaison avec au moins Ctrl, Alt ou Win (Maj en plus si tu veux).';
  } else if (pendingShortcut && pendingShortcut !== config.shortcut) {
    status.className = 'small muted';
    status.textContent = 'Clique sur « Enregistrer » pour activer ce raccourci.';
  } else if (platform.shortcut_error) {
    status.className = 'small error-text';
    status.textContent = `✗ ${platform.shortcut_error}`;
  } else if (platform.shortcut_active) {
    status.className = 'small ok-text';
    status.textContent = '✓ Raccourci actif dans toutes les applications.';
  } else {
    status.textContent = '';
  }
}

async function startRecording() {
  if (recording) return;
  recording = true;
  renderShortcut();
  // Le raccourci actuel ne doit pas se déclencher pendant qu'on en choisit un nouveau.
  try { await invoke('suspend_shortcut'); } catch { /* ignoré */ }
}

async function stopRecording() {
  if (!recording) return;
  recording = false;
  try { platform.shortcut_error = await invoke('resume_shortcut'); } catch { /* ignoré */ }
  renderShortcut();
}

function onRecordKey(e) {
  if (!recording) return;
  e.preventDefault();
  e.stopPropagation();
  if (e.key === 'Escape') { stopRecording(); return; }
  const key = keyFromEvent(e);
  if (!key) return; // touche de modification seule : on attend la suite
  const mods = [];
  if (e.ctrlKey) mods.push('Ctrl');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  if (e.metaKey) mods.push(isMac() ? 'Cmd' : 'Super');
  // Maj seule ne suffit pas : taper une majuscule déclencherait Coquille.
  if (!e.ctrlKey && !e.altKey && !e.metaKey) {
    $('shortcut-status').className = 'small error-text';
    $('shortcut-status').textContent = 'Utilise au moins Ctrl, Alt ou Win (Maj seule ne suffit pas).';
    return;
  }
  pendingShortcut = [...mods, key].join('+');
  stopRecording();
}

// ---------------------------------------------------------------------------
// Événements
// ---------------------------------------------------------------------------
function bindEvents() {
  el.text.addEventListener('input', () => {
    const now = el.text.value;
    rebase(lastText, now);
    lastText = now;
    hidePopover();
    dirty = true;
    render();
    if (config.live) scheduleCheck(700);
  });
  el.text.addEventListener('scroll', () => { syncScroll(); hidePopover(); });
  el.text.addEventListener('click', () => {
    const pos = el.text.selectionStart;
    if (el.text.selectionEnd !== pos) { hidePopover(); return; }
    const i = issueAt(pos);
    activeKey = i ? i.key : null;
    render();
    if (i) {
      showPopover(i);
      el.issues.querySelector(`.card[data-key="${i.key}"]`)?.scrollIntoView({ block: 'nearest' });
    } else hidePopover();
  });
  el.text.addEventListener('keyup', (e) => {
    if (!e.key.startsWith('Arrow') && e.key !== 'Home' && e.key !== 'End') return;
    const i = issueAt(el.text.selectionStart);
    const key = i ? i.key : null;
    if (key !== activeKey) { activeKey = key; hidePopover(); render(); }
  });

  document.addEventListener('mousedown', (e) => {
    if (!el.popover.hidden && !el.popover.contains(e.target) && e.target !== el.text) hidePopover();
  });
  document.addEventListener('keydown', (e) => {
    if (el.dialog.open) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      if (!el.popover.hidden) hidePopover();
      else invoke('hide_window');
    } else if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      applyToSource();
    }
  });
  window.addEventListener('resize', () => { hidePopover(); syncScroll(); });

  el.status.addEventListener('click', runCheck);
  el.lang.addEventListener('change', changeLanguage);
  el.fixAll.addEventListener('click', fixAll);
  el.copy.addEventListener('click', copyText);
  el.apply.addEventListener('click', applyToSource);
  $('btn-settings').addEventListener('click', openSettings);
  $('notice-close').addEventListener('click', hideNotice);

  el.form.addEventListener('submit', saveSettings);
  $('btn-cancel').addEventListener('click', () => el.dialog.close());
  $('btn-test').addEventListener('click', testServer);
  $('btn-public').addEventListener('click', () => { el.form.elements.server_url.value = PUBLIC_SERVER; testServer(); });
  $('shortcut-rec').addEventListener('click', startRecording);
  document.addEventListener('keydown', onRecordKey, true);
  // Échap pendant l'enregistrement annule la saisie, pas toute la fenêtre de paramètres.
  el.dialog.addEventListener('cancel', (e) => { if (recording) e.preventDefault(); });
  el.dialog.addEventListener('close', () => { if (recording) stopRecording(); });
}

async function init() {
  bindEvents();
  [config, platform] = await Promise.all([invoke('get_config'), invoke('platform_info')]);
  el.apply.title = `Coller le texte corrigé dans l'application d'origine (${modLabel()}+Entrée)`;
  fillLanguages(FALLBACK_LANGS);
  loadServerLanguages();
  updateSource();
  updateHint();
  await listen('capture', (e) => applyCapture(e.payload));
  await listen('open-settings', () => openSettings());
  const pending = await invoke('take_pending');
  if (pending) applyCapture(pending);
  if (platform.shortcut_notice) showNotice(platform.shortcut_notice, 'info');
  else if (platform.shortcut_error) showNotice(platform.shortcut_error, 'warn');
  render();
  el.text.focus();
}

init().catch((e) => showNotice(`Erreur d'initialisation : ${e}`, 'error'));
