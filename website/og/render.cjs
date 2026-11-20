// Renders og/card.html to public/og.png at 1200 x 630, the size Open Graph
// and X's large card expect. Needs Playwright with Chromium:
//   node og/render.cjs
const { chromium } = require('playwright');
const path = require('node:path');

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1200, height: 630 }, deviceScaleFactor: 1 });
  await page.goto('file://' + path.join(__dirname, 'card.html'));
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path: path.join(__dirname, '..', 'public', 'og.png'), type: 'png' });
  await browser.close();
  console.log('wrote public/og.png');
})();
