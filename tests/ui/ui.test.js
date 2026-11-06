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
    await page.click('input[name=shortcut]');
    await page.keyboard.press('Control+Alt+KeyK');
    assert.strictEqual(await page.inputValue('input[name=shortcut]'), 'Ctrl+Alt+K');
    await page.click('#btn-test');
    await page.waitForFunction(() => document.getElementById('test-result').textContent.startsWith('Connexion réussie'));
    await page.screenshot({ path: `${OUT}/ui-settings.png` });
    await page.click('dialog button[type=submit]');
    await page.waitForSelector('dialog:not([open])', { state: 'attached' });
    const saved = await page.evaluate(() => window.__calls.filter((c) => c[0] === 'save_config').pop()[1].config.shortcut);
    assert.strictEqual(saved, 'Ctrl+Alt+K');

    // Petite fenêtre : pas de débordement horizontal
    await page.setViewportSize({ width: 640, height: 480 });
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
    assert.ok(!overflow, 'pas de défilement horizontal à 640px');
    await page.close();
  }
  await browser.close();
  if (errors.length) { console.error('Erreurs JS :', errors); process.exit(1); }
  console.log('UI OK');
})().catch((e) => { console.error(e); process.exit(1); });
