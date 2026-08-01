import puppeteer from 'puppeteer-core';
import { wsUrl } from './_helpers.mjs';

const enabled = await fingerprint(wsUrl('/session', { stealth: 'true' }));
const disabled = await fingerprint(wsUrl('/session', { stealth: 'false' }));

assertEnabled(enabled);

if (process.env.EIGER_ASSERT_STEALTH_OFF === 'true') {
  const differs =
    enabled.webdriver !== disabled.webdriver ||
    enabled.plugins !== disabled.plugins ||
    enabled.webglVendor !== disabled.webglVendor ||
    enabled.hasChromeRuntime !== disabled.hasChromeRuntime;

  if (!differs) {
    throw new Error(`expected stealth=false fingerprint to differ: ${JSON.stringify({ enabled, disabled })}`);
  }
}

console.log(JSON.stringify({ enabled, disabled }));

async function fingerprint(endpoint) {
  const browser = await puppeteer.connect({ browserWSEndpoint: endpoint });
  try {
    const page = await browser.newPage();
    await page.goto('data:text/html,<title>eiger-stealth</title>');
    return await page.evaluate(() => {
      const canvas = document.createElement('canvas');
      const gl = canvas.getContext('webgl') || canvas.getContext('experimental-webgl');

      return {
        webdriver: navigator.webdriver === undefined ? 'undefined' : String(navigator.webdriver),
        languages: Array.from(navigator.languages || []),
        plugins: navigator.plugins ? navigator.plugins.length : 0,
        mimeTypes: navigator.mimeTypes ? navigator.mimeTypes.length : 0,
        hasChromeRuntime: Boolean(window.chrome && window.chrome.runtime),
        notificationPermission: Notification.permission,
        webglVendor: gl ? gl.getParameter(37445) : null,
        webglRenderer: gl ? gl.getParameter(37446) : null,
      };
    });
  } finally {
    await browser.close();
  }
}

function assertEnabled(value) {
  const failures = [];
  if (value.webdriver !== 'undefined') failures.push(`webdriver=${value.webdriver}`);
  if (!value.languages.includes('en-US')) failures.push(`languages=${JSON.stringify(value.languages)}`);
  if (value.plugins < 1) failures.push(`plugins=${value.plugins}`);
  if (value.mimeTypes < 1) failures.push(`mimeTypes=${value.mimeTypes}`);
  if (!value.hasChromeRuntime) failures.push('missing chrome.runtime');
  if (value.webglVendor !== null && value.webglVendor !== 'Intel Inc.') {
    failures.push(`webglVendor=${value.webglVendor}`);
  }

  if (failures.length > 0) {
    throw new Error(`stealth baseline failed: ${failures.join(', ')}`);
  }
}
