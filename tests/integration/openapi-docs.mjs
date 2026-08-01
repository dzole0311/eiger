import { withEigerServer } from './_server.mjs';

await withEigerServer({}, async ({ httpBaseUrl }) => {
  const specResponse = await fetch(`${httpBaseUrl}/openapi.json`);
  if (!specResponse.ok) {
    throw new Error(`GET /openapi.json failed: ${specResponse.status} ${await specResponse.text()}`);
  }

  const spec = await specResponse.json();
  for (const path of [
    '/scrape',
    '/screenshot',
    '/pdf',
    '/sessions',
    '/sessions/{id}',
  ]) {
    if (!spec.paths?.[path]) {
      throw new Error(`expected OpenAPI spec to include ${path}`);
    }
  }

  const docsResponse = await fetch(`${httpBaseUrl}/docs/`);
  if (!docsResponse.ok) {
    throw new Error(`GET /docs/ failed: ${docsResponse.status} ${await docsResponse.text()}`);
  }
  const docs = await docsResponse.text();
  if (!docs.includes('SwaggerUIBundle') && !docs.includes('swagger-ui')) {
    throw new Error('expected Swagger UI HTML');
  }

  console.log(JSON.stringify({ ok: true }));
});
