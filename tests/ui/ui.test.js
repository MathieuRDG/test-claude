const { chromium } = require('playwright');
const assert = require('assert');
const path = require('path');
const OUT = process.argv[2] || require('os').tmpdir();
const PAGE = 'file://' + path.resolve(__dirname, '../../src/index.html');
(async () => {
  // CHROMIUM_PATH : navigateur déjà installé (sinon, celui téléchargé par « npx playwright install »).
  const browser = await chromium.launch(process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {});
  const errors = [];
  for (const scheme of ['light', 'dark']) {
    const page = await browser.newPage({ viewport: { width: 1000, height: 640 }, colorScheme: scheme, deviceScaleFactor: scheme === 'light' ? 2 : 1 });
    page.on('pageerror', (e) => errors.push(e.message));
    page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
    await page.addInitScript({ path: path.join(__dirname, 'mock-tauri.js') });
    await page.goto(PAGE);
    await page.waitForFunction(() => document.querySelector('#lang option'));
    const text = 'Bonjoru les amis est là ? Mon serveur Proxmocks est très très rapide.';
    await page.fill('#text', text);
    await page.waitForFunction(() => document.querySelectorAll('#issues .card').length === 5, null, { timeout: 4000 });
    const marks = await page.$$eval('#highlights mark', (ms) => ms.map((m) => [m.className, m.textContent]));
    assert.deepStrictEqual(marks.map((m) => m[1]), ['Bonjoru', 'est', ' ?', 'Proxmocks', 'très très']);
    assert.strictEqual(await page.textContent('#status'), '5 corrections suggérées');
    if (scheme === 'dark') { await page.screenshot({ path: `${OUT}/ui-dark.png` }); await page.close(); continue; }

    // Alignement : chaque <mark> doit tomber exactement sous le texte de la zone de saisie.
    const align = await page.evaluate(() => {
      const ta = document.getElementById('text'); const hl = document.getElementById('highlights');
      return { taW: ta.clientWidth, bdW: document.getElementById('backdrop').clientWidth, taH: ta.scrollHeight, bdH: document.getElementById('backdrop').scrollHeight };
    });
    assert.strictEqual(align.taW, align.bdW, 'largeurs identiques');
    assert.strictEqual(align.taH, align.bdH, 'hauteurs identiques');

    // Clic sur le mot fautif -> bulle
    const box = await page.locator('#highlights mark').first().boundingBox();
    await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
    await page.waitForSelector('#popover:not([hidden])');
    await page.screenshot({ path: `${OUT}/ui-popover.png` });
    await page.click('#popover .sugg >> text=Bonjour');
    assert.ok((await page.inputValue('#text')).startsWith('Bonjour les amis'));
    // Le soulignement des autres fautes doit suivre le décalage avant même la revérification.
    const est = await page.$$eval('#highlights mark', (ms) => ms.map((m) => m.textContent));
    assert.ok(est.includes('est') && !est.includes('Bonjoru'), 'rebase ok: ' + est);

    // Annulation via Ctrl+Z conservée
    await page.keyboard.press('Control+z');
    assert.ok((await page.inputValue('#text')).startsWith('Bonjoru'), 'undo fonctionne');
    await page.keyboard.press('Control+Shift+z').catch(() => {});
    await page.fill('#text', text);
    await page.waitForFunction(() => document.querySelectorAll('#issues .card').length === 5);

    // Dictionnaire
    await page.click('#issues .card:has-text("Proxmocks")');
    await page.click('#issues .card.active >> text=Ajouter au dictionnaire');
    await page.waitForFunction(() => document.querySelectorAll('#issues .card').length === 4);

    // Tout corriger (hors style)
    await page.click('#btn-fix-all');
    const fixed = await page.inputValue('#text');
    assert.strictEqual(fixed, 'Bonjour les amis sont là ? Mon serveur Proxmocks est très très rapide.');
    await page.waitForFunction(() => document.querySelectorAll('#issues .card').length === 1);

    // Capture depuis une autre application
    await page.evaluate(() => window.__emit('capture', { id: 1, text: 'Les amis est contents ?', from_selection: true, notice: null }));
    await page.waitForSelector('#btn-apply:not([hidden])');
    // Aucune ancienne correction ne doit rester affichée pendant la nouvelle vérification.
    const stale = await page.$$eval('#issues .card, #highlights mark', (n) => n.map((x) => x.textContent).join('|'));
    assert.ok(!stale.includes('très'), 'anciennes corrections encore visibles : ' + stale);
    assert.strictEqual(await page.textContent('#source'), 'Sélection capturée');
    await page.waitForFunction(() => [...document.querySelectorAll('#highlights mark')].map((m) => m.textContent).join() === ' ?');
    await page.screenshot({ path: `${OUT}/ui-capture.png` });
    await page.keyboard.press('Control+Enter');
    await page.waitForFunction(() => window.__calls.some((c) => c[0] === 'apply_text'));
    await page.waitForSelector('#btn-apply[hidden]', { state: 'attached' });

    // Capture sans sélection -> bandeau
    await page.evaluate(() => window.__emit('capture', { id: 2, text: '', from_selection: false, notice: 'Aucun texte sélectionné.' }));
    await page.waitForSelector('#notice:not([hidden])');

    // Paramètres + saisie du raccourci
    await page.click('#btn-settings');
    await page.waitForSelector('dialog[open]');
    assert.ok((await page.textContent('#shortcut-status')).includes('actif'));
    await page.click('#shortcut-rec');
    await page.waitForSelector('#shortcut-rec.recording');
    assert.ok(await page.evaluate(() => window.__calls.some((c) => c[0] === 'suspend_shortcut')), 'raccourci suspendu');
    await page.keyboard.press('Escape'); // annule l'enregistrement sans fermer la fenêtre
    assert.ok(await page.evaluate(() => document.getElementById('settings').open), 'Échap ne ferme pas les paramètres');
    assert.strictEqual(await page.textContent('#shortcut-keys'), 'Ctrl + Alt + Espace');
    await page.click('#shortcut-rec');
    await page.keyboard.press('KeyQ'); // aucune touche de modification -> refusé
    assert.ok((await page.textContent('#shortcut-status')).includes('Ctrl, Alt ou Win'));
    await page.keyboard.press('Shift+KeyQ'); // Maj seule -> refusé aussi
    assert.ok(await page.isVisible('#shortcut-rec.recording'), 'toujours en enregistrement');
    await page.keyboard.press('Control+Alt+KeyK');
    assert.strictEqual(await page.textContent('#shortcut-keys'), 'Ctrl + Alt + K');
    assert.ok((await page.textContent('#shortcut-status')).includes('Enregistrer'));
    assert.ok(!(await page.isHidden('#bubble-settings')), 'options de la bulle visibles sous Windows');
    await page.click('#btn-test');
    await page.waitForFunction(() => document.getElementById('test-result').textContent.startsWith('Connexion réussie'));
    await page.screenshot({ path: `${OUT}/ui-settings.png` });
    await page.click('dialog button[type=submit]');
    await page.waitForSelector('dialog:not([open])', { state: 'attached' });
    const saved = await page.evaluate(() => window.__calls.filter((c) => c[0] === 'save_config').pop()[1].config);
    assert.strictEqual(saved.shortcut, 'Ctrl+Alt+K');
    assert.strictEqual(saved.bubble, true);
    assert.deepStrictEqual(saved.bubble_excluded_apps, ['KeePassXC.exe']);

    // Ouverture depuis la bulle : champ entier
    await page.evaluate(() => window.__emit('capture', { id: 3, text: 'Texte du champ', from_selection: true, mode: 'field', notice: null }));
    await page.waitForFunction(() => document.getElementById('source').textContent === 'Champ capturé');

    // Petite fenêtre : pas de débordement horizontal
    await page.setViewportSize({ width: 640, height: 480 });
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
    assert.ok(!overflow, 'pas de défilement horizontal à 640px');
    await page.close();
  }
  // ---- Fenêtre « bulle » ----
  const bpage = await browser.newPage({ viewport: { width: 30, height: 30 }, deviceScaleFactor: 2 });
  bpage.on('pageerror', (e) => errors.push('bulle: ' + e.message));
  await bpage.addInitScript({ path: path.join(__dirname, 'mock-tauri.js') });
  const issue = (o) => ({ offset: 0, length: 7, original: 'Bonjoru', message: 'Faute de frappe possible trouvée.', short_message: '',
    replacements: ['Bonjour', 'Bonjours'], rule_id: 'FR_SPELLING_RULE', rule_description: 'x', category: '', kind: 'orthographe', ...o });
  await bpage.addInitScript(() => { window.__bubbleState = { status: 'checking', issues: [], error: null, message: null, expanded: false, stale: true, app: 'chrome.exe' }; });
  await bpage.goto(PAGE.replace('index.html', 'bubble.html'));
  await bpage.waitForSelector('#badge.checking');
  const state = { status: 'issues', issues: [issue(), issue({ offset: 12, length: 3, original: 'est', kind: 'grammaire', replacements: ['sont'], rule_id: 'AGREEMENT' })],
    error: null, message: null, expanded: false, stale: false, app: 'chrome.exe' };
  await bpage.evaluate((s) => window.__emit('bubble-state', s), state);
  assert.strictEqual(await bpage.textContent('#count'), '2');
  await bpage.screenshot({ path: `${OUT}/bubble-badge.png` });
  await bpage.click('#badge');
  assert.ok(await bpage.evaluate(() => window.__calls.some((c) => c[0] === 'bubble_expand' && c[1].expanded)));
  await bpage.setViewportSize({ width: 360, height: 430 });
  await bpage.evaluate((s) => window.__emit('bubble-state', { ...s, expanded: true }), state);
  await bpage.waitForSelector('#panel:not([hidden])');
  assert.strictEqual(await bpage.locator('.card').count(), 2);
  await bpage.screenshot({ path: `${OUT}/bubble-panel.png` });
  await bpage.click('.card.grammaire .sugg');
  const applied = await bpage.evaluate(() => window.__calls.filter((c) => c[0] === 'bubble_apply').pop()[1]);
  assert.deepStrictEqual(applied, { offset: 12, length: 3, original: 'est', replacement: 'sont' });
  await bpage.click('.card.orthographe >> text=Ignorer');
  assert.ok(await bpage.evaluate(() => window.__calls.some((c) => c[0] === 'bubble_ignore' && c[1].ruleId === 'FR_SPELLING_RULE')));
  await bpage.evaluate((s) => window.__emit('bubble-state', { ...s, status: 'ok', issues: [], expanded: false }), state);
  assert.strictEqual(await bpage.textContent('#count'), '✓');
  await bpage.close();

  await browser.close();
  if (errors.length) { console.error('Erreurs JS :', errors); process.exit(1); }
  console.log('UI OK');
})().catch((e) => { console.error(e); process.exit(1); });
