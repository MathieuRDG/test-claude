'use strict';

// Fenêtre « bulle » : pastille près du champ de saisie actif, et panneau de corrections.
const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
const listen = (event, cb) => window.__TAURI__.event.listen(event, cb);
const $ = (id) => document.getElementById(id);

const KIND_LABEL = { orthographe: 'Orthographe', grammaire: 'Grammaire', typographie: 'Typographie', style: 'Style' };
const plural = (n, one, many) => `${n} ${n > 1 ? many : one}`;

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

function displaySuggestion(r) {
  if (r === '') return '(supprimer)';
  if (r.trim() === '') return r.includes(' ') ? 'espace fine insécable' : r.includes(' ') ? 'espace insécable' : 'espace';
  return r.replace(/[  ]/g, '·');
}

function card(i) {
  const sugg = h('div', { class: 'suggestions' });
  if (i.replacements.length) {
    for (const r of i.replacements) {
      sugg.append(h('button', {
        type: 'button', class: 'sugg', text: displaySuggestion(r), title: `Remplacer par « ${r} »`,
        onclick: () => invoke('bubble_apply', { offset: i.offset, length: i.length, original: i.original, replacement: r }),
      }));
    }
  } else {
    sugg.append(h('span', { class: 'nosugg', text: 'Aucune suggestion' }));
  }
  const actions = h('div', { class: 'actions' }, [
    h('button', { type: 'button', text: 'Ignorer', onclick: () => invoke('bubble_ignore', { ruleId: i.rule_id, original: i.original }) }),
    i.kind === 'orthographe'
      ? h('button', {
        type: 'button', text: 'Ajouter au dictionnaire',
        onclick: async () => { await invoke('add_to_dictionary', { word: i.original }); invoke('bubble_recheck'); },
      })
      : null,
    h('button', {
      type: 'button', text: 'Désactiver la règle', title: `${i.rule_id} — ${i.rule_description}`,
      onclick: async () => { await invoke('disable_rule', { ruleId: i.rule_id }); invoke('bubble_recheck'); },
    }),
  ]);
  return h('div', { class: `card ${i.kind}` }, [
    h('div', { class: 'card-head' }, [
      h('span', { class: 'kind', text: KIND_LABEL[i.kind] || i.kind }),
      h('span', { class: 'orig', text: i.original.trim() ? i.original : '␣', title: i.original }),
    ]),
    h('div', { class: 'msg', text: i.message }),
    sugg,
    actions,
  ]);
}

function render(s) {
  if (!s) return;
  document.body.classList.toggle('expanded', s.expanded);
  const n = s.issues.length;
  const onlyStyle = n > 0 && s.issues.every((i) => i.kind === 'style');

  // Pastille
  const badge = $('badge');
  badge.className = `badge ${s.status === 'issues' && onlyStyle ? 'style' : s.status}${s.stale && s.status !== 'checking' ? ' stale' : ''}`;
  $('count').textContent = s.status === 'issues' ? (n > 9 ? '9+' : String(n)) : s.status === 'ok' ? '✓' : s.status === 'error' ? '!' : '';
  badge.title = s.status === 'issues' ? `${plural(n, 'correction', 'corrections')} — cliquer pour voir`
    : s.status === 'ok' ? 'Aucune faute détectée'
      : s.status === 'error' ? (s.error || 'Erreur') : 'Vérification…';

  // Panneau
  $('panel').hidden = !s.expanded;
  if (!s.expanded) return;
  $('summary').textContent = s.status === 'checking' ? 'vérification…'
    : `${plural(n, 'correction', 'corrections')}${s.stale ? ' · texte modifié' : ''}${s.app ? ` · ${s.app.replace(/\.exe$/i, '')}` : ''}`;
  const msg = s.message || s.error;
  $('message').hidden = !msg;
  $('message').textContent = msg || '';
  const list = $('list');
  if (!n) {
    list.replaceChildren(h('div', { class: 'empty' }, [
      s.status === 'ok' ? h('span', { class: 'big', text: '✓' }) : null,
      s.status === 'ok' ? 'Aucune faute détectée.' : s.status === 'error' ? 'Vérification impossible.' : 'Vérification…',
    ]));
  } else {
    list.replaceChildren(...s.issues.map(card));
  }
}

$('badge').addEventListener('click', () => invoke('bubble_expand', { expanded: true }));
$('close').addEventListener('click', () => invoke('bubble_expand', { expanded: false }));
$('open').addEventListener('click', () => invoke('bubble_open_editor'));

(async () => {
  await listen('bubble-state', (e) => render(e.payload));
  render(await invoke('bubble_get_state'));
})();
