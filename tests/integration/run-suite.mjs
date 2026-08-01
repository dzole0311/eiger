import { runNodeScript } from './_helpers.mjs';

const scripts = [
  'compose-token.mjs',
  'deployment-docs.mjs',
  'cors-policy.mjs',
  'rate-limit.mjs',
  'body-limit.mjs',
  'puppeteer-smoke.mjs',
  'playwright-smoke.mjs',
  'readiness.mjs',
  'launch-queue-timeout.mjs',
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
