import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const readme = await readFile(path.join(repoRoot, 'README.md'), 'utf8');

const requiredText = [
  '## Deployment',
  'Eiger does not terminate TLS',
  'TLS-terminating reverse proxy',
  'nginx, Caddy or Traefik',
  'X-Forwarded-Proto',
];

for (const text of requiredText) {
  if (!readme.includes(text)) {
    throw new Error(`README.md missing deployment text: ${text}`);
  }
}

console.log(JSON.stringify({ ok: true, checked: requiredText }));
