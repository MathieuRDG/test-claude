window.__calls = [];
window.__listeners = {};
window.__emit = (ev, payload) => (window.__listeners[ev] || []).forEach((cb) => cb({ payload }));
const RULES = [
  { re: /Bonjoru/g, kind: 'orthographe', rule: 'FR_SPELLING_RULE', msg: 'Faute de frappe possible trouvée.', rep: ['Bonjour', 'Bonjours'] },
  { re: /Proxmocks/g, kind: 'orthographe', rule: 'FR_SPELLING_RULE', msg: 'Faute de frappe possible trouvée.', rep: ['Proxmox'] },
  { re: /(?<=les amis )est/g, kind: 'grammaire', rule: 'AGREEMENT_SUJET_VERBE', msg: 'Le verbe doit s’accorder avec le sujet « les amis ».', rep: ['sont'] },
  { re: / \?/g, kind: 'typographie', rule: 'FRENCH_WHITESPACE', msg: 'Une espace fine insécable est requise avant « ? ».', rep: [' ?'] },
  { re: /très très/g, kind: 'style', rule: 'FR_REPEATEDWORDS', msg: 'Répétition : envisagez de supprimer un mot.', rep: ['très'] },
];
let config = { server_url: 'https://api.languagetool.org', language: 'fr', shortcut: 'Ctrl+Alt+Space', picky: false, live: true, restore_clipboard: true, dictionary: [], disabled_rules: [], username: '', api_key: '', bubble: true, bubble_excluded_apps: ['KeePassXC.exe'] };
const handlers = {
  get_config: () => config,
  platform_info: () => ({ os: 'windows', wayland: false, shortcut_active: true, shortcut_error: null, shortcut_notice: null, bubble_supported: true, config_path: '/home/user/.config/org.coquille.correcteur/config.json', version: '0.1.0' }),
  take_pending: () => null,
  list_languages: () => [{ name: 'French', code: 'fr', longCode: 'fr' }, { name: 'English (US)', code: 'en', longCode: 'en-US' }, { name: 'German (Germany)', code: 'de', longCode: 'de-DE' }],
  check_text: ({ text }) => {
    const issues = [];
    for (const r of RULES) {
      if (config.disabled_rules.includes(r.rule)) continue;
      for (const m of text.matchAll(r.re)) {
        if (r.kind === 'orthographe' && config.dictionary.some((d) => d.toLowerCase() === m[0].toLowerCase())) continue;
        issues.push({ offset: m.index, length: m[0].length, original: m[0], message: r.msg, short_message: '', replacements: r.rep, rule_id: r.rule, rule_description: r.msg, category: '', kind: r.kind });
      }
    }
    return new Promise((res) => setTimeout(() => res({ issues, language: 'Français', language_code: 'fr' }), 80));
  },
  save_config: ({ config: c }) => (config = { ...c, server_url: c.server_url.trim().replace(/\/+$/, '') }),
  add_to_dictionary: ({ word }) => (config = { ...config, dictionary: [...config.dictionary, word] }),
  disable_rule: ({ ruleId }) => (config = { ...config, disabled_rules: [...config.disabled_rules, ruleId] }),
  copy_text: () => null, suspend_shortcut: () => null, resume_shortcut: () => null,
  bubble_get_state: () => window.__bubbleState || null, bubble_expand: () => null, bubble_apply: () => null,
  bubble_ignore: () => null, bubble_recheck: () => null, bubble_open_editor: () => null, hide_window: () => null, get_autostart: () => false, set_autostart: () => null,
  apply_text: () => ({ pasted: true, message: null }),
  test_server: ({ url }) => `Connexion réussie à ${url} : 31 langues disponibles, dont le français.`,
};
window.__TAURI__ = {
  core: { invoke: async (cmd, args) => { window.__calls.push([cmd, args]); if (!handlers[cmd]) throw new Error('unknown ' + cmd); return handlers[cmd](args || {}); } },
  event: { listen: async (ev, cb) => { (window.__listeners[ev] ||= []).push(cb); return () => {}; } },
};
