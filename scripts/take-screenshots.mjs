import { chromium } from 'playwright';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import { mkdirSync } from 'fs';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, '..');
const ui = join(root, 'ui', 'panel.html');
const out = join(root, 'screenshots');
mkdirSync(out, { recursive: true });

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });

await page.goto('file://' + ui);

// Wait for UI to initialize
await page.waitForTimeout(1500);

// Helper to switch tabs
async function goTab(tab) {
  await page.evaluate((t) => {
    if (typeof window.__setTab === 'function') {
      window.__setTab(t);
    } else {
      const btn = document.querySelector(`nav button[data-tab="${t}"]`);
      if (btn) btn.click();
    }
  }, tab);
  await page.waitForTimeout(800);
}

// Library
await goTab('games');
await page.screenshot({ path: join(out, 'library.jpg'), type: 'jpeg', quality: 90, fullPage: false });

// Mashup Browser
await goTab('mashups');
await page.waitForTimeout(1000);
await page.screenshot({ path: join(out, 'mashup-browser.jpg'), type: 'jpeg', quality: 90, fullPage: false });

// Mashup Details - try to click first mashup card if exists
await goTab('mashups');
await page.waitForTimeout(1000);
const clicked = await page.evaluate(() => {
  const card = document.querySelector('#mashups .card, .mashup-card, [data-mashup]');
  if (card) { card.click(); return true; }
  return false;
});
await page.waitForTimeout(1200);
await page.screenshot({ path: join(out, 'mashup-details.jpg'), type: 'jpeg', quality: 90, fullPage: false });
// Close details if possible
await page.evaluate(() => {
  const close = document.querySelector('#mashup-detail .close, .detail-close, [aria-label="Close"]');
  if (close) close.click();
});
await page.waitForTimeout(500);

// Installed
await goTab('installed');
await page.waitForTimeout(800);
await page.screenshot({ path: join(out, 'installed.jpg'), type: 'jpeg', quality: 90, fullPage: false });

// Settings
await goTab('settings');
await page.waitForTimeout(800);
await page.screenshot({ path: join(out, 'settings.jpg'), type: 'jpeg', quality: 90, fullPage: false });

// Diagnostics
await goTab('diagnostics');
await page.waitForTimeout(800);
await page.screenshot({ path: join(out, 'diagnostics.jpg'), type: 'jpeg', quality: 90, fullPage: false });

await browser.close();
console.log('Screenshots saved to', out);
