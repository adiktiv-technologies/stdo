import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { stat, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { join } from 'node:path';

const [url, work] = process.argv.slice(2);
const { chromium } = createRequire(join(work, 'capture.cjs'))('playwright');
const parent = 'examples/11-composable-subworkflows.yaml';
const child = 'examples/07-ollama-gemma4-phonon-speech.yaml';
const byteLimit = 180_000;

async function assertInsidePanel(locator, label) {
  const fullyVisible = await locator.evaluate(element => {
    const item = element.getBoundingClientRect();
    const panel = element.closest('.catalog-list, .catalog-detail').getBoundingClientRect();
    return item.top >= panel.top - 1 && item.bottom <= panel.bottom + 1;
  });
  assert.ok(fullyVisible, `${label} must fit in its scroll panel`);
}

async function capture(page, name) {
  const raw = join(work, 'raw.png');
  const output = join(work, name + '.png');
  await writeFile(raw, await page.screenshot({ animations: 'disabled', caret: 'hide' }));
  // Start with a near-retina image. Reduce dimensions and palette only as needed.
  for (const [width, colors] of [[2160, 192], [1920, 128], [1440, 128], [1440, 64]]) {
    const filter = `scale=${width}:-1:flags=lanczos,split[pixels][colors];` +
      `[colors]palettegen=max_colors=${colors}:reserve_transparent=0[palette];` +
      '[pixels][palette]paletteuse=dither=none';
    execFileSync('ffmpeg', [
      '-nostdin', '-hide_banner', '-loglevel', 'error', '-y', '-i', raw,
      '-filter_complex_threads', '1', '-filter_complex', filter,
      '-frames:v', '1', '-compression_level', '9', '-update', '1', output,
    ], { stdio: 'inherit' });
    if ((await stat(output)).size < byteLimit) return;
  }
  throw new Error(`${name} exceeds ${byteLimit} bytes`);
}

const ready = await fetch(`${url}/api/workflows`, { signal: AbortSignal.timeout(10_000) });
assert.ok(ready.ok, 'Studio workflow API must respond');
assert.equal((await ready.json()).selected, parent);

const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage({
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 2,
    colorScheme: 'dark',
    locale: 'en-US',
    reducedMotion: 'reduce',
  });
  page.setDefaultTimeout(10_000);
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error') errors.push(message.text());
  });

  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await page.evaluate(() => document.fonts.ready);
  await page.getByText('composable-subworkflows', { exact: true }).waitFor();
  await page.getByRole('button', { name: 'Requirements', exact: true }).click();
  await page.getByRole('table', { name: 'CLI Binaries' }).waitFor();
  const openLinks = page.locator('#nodes-layer').getByRole('button', { name: 'Open ↗' });
  await openLinks.nth(1).waitFor();
  assert.equal(await openLinks.count(), 2);
  await page.mouse.move(0, 0);
  await capture(page, 'studio-overview-subworkflows');

  await page.getByRole('button', { name: '+ Command Step', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Add Command Step' });
  await dialog.getByRole('button', { name: '🛡️ DSCR', exact: true }).click();
  const presets = dialog.locator('.catalog-preset');
  await presets.nth(6).waitFor();
  assert.equal(await presets.count(), 7);
  await dialog.getByRole('button', { name: /DSCR Verify \.nex Cryptographic Manifest/ }).click();
  await dialog.getByRole('link', { name: /nexdatas\.com/ }).waitFor();
  assert.match(await dialog.locator('pre').innerText(), /"--nex"/);
  await assertInsidePanel(presets.nth(6), 'The seventh DSCR preset');
  await page.mouse.move(0, 0);
  await capture(page, 'studio-catalog-dscr');

  await dialog.getByRole('button', { name: '🔗 Sub-Workflows', exact: true }).click();
  const workflow = dialog.getByLabel('Sub-workflow file');
  await workflow.selectOption(child);
  await dialog.getByLabel('Override prompt').waitFor();
  await dialog.getByLabel('Override output_wav').waitFor();
  const preview = dialog.locator('pre');
  assert.deepEqual(JSON.parse(await preview.innerText()).cmd, ['stdo', 'run', '-f', child]);
  await assertInsidePanel(preview, 'The sub-workflow command preview');
  await page.mouse.move(0, 0);
  await capture(page, 'studio-catalog-subworkflows');

  assert.deepEqual(errors, [], 'Studio must render without JavaScript errors');
} finally {
  await browser.close();
}
