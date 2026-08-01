import { runNodeScript } from './_helpers.mjs';

const scripts = [
  'puppeteer-smoke.mjs',
  'playwright-smoke.mjs',
  'stealth-check.mjs',
  'process-reap.mjs',
  'adversarial-disconnect.mjs',
];

if (process.env.EIGER_RUN_LONG_TESTS === 'true') {
  scripts.push('load-test.mjs');
  scripts.push('rss-limit.mjs');
}

for (const script of scripts) {
  console.log(`\n--- ${script} ---`);
  await runNodeScript(script);
}
