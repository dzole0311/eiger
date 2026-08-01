import { httpUrl } from './_helpers.mjs';

const expectedOrigin = process.env.EIGER_EXPECT_CORS_ORIGIN;
const origin = expectedOrigin ?? 'https://cross-origin.example';

const response = await fetch(httpUrl('/health'), {
  headers: { origin },
});

if (!response.ok) {
  throw new Error(`health request failed: ${response.status}`);
}

assertCorsHeader(response.headers.get('access-control-allow-origin'), 'GET /health');

const preflight = await fetch(httpUrl('/sessions'), {
  method: 'OPTIONS',
  headers: {
    origin,
    'access-control-request-method': 'POST',
    'access-control-request-headers': 'authorization,content-type',
  },
});

assertCorsHeader(preflight.headers.get('access-control-allow-origin'), 'OPTIONS /sessions');

console.log(JSON.stringify({
  ok: true,
  origin,
  mode: expectedOrigin ? 'allowlist' : 'default-deny',
}));

function assertCorsHeader(actual, label) {
  if (expectedOrigin) {
    if (actual !== expectedOrigin) {
      throw new Error(`${label} expected CORS origin ${expectedOrigin}, got ${actual}`);
    }
    return;
  }

  if (actual !== null) {
    throw new Error(`${label} should not allow cross-origin requests, got ${actual}`);
  }
}
