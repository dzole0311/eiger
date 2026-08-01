# Verification

Run the Rust checks:

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Run the standard browser integration suite against a local server:

```bash
EIGER_BIND_ADDR=127.0.0.1:3102 \
EIGER_CHROME_NO_SANDBOX=false \
EIGER_MAINTENANCE_INTERVAL_SECS=1 \
EIGER_CDP_HEALTH_INTERVAL_SECS=2 \
cargo run -p eiger-server

cd tests/integration
npm install
EIGER_HOST=127.0.0.1:3102 npm run suite
```

The standard suite runs:

- Compose token configuration check.
- Deployment docs check.
- CORS default-deny check.
- Rate-limit check.
- Request body limit check.
- Puppeteer CDP smoke test.
- Playwright CDP smoke test.
- Readiness check.
- Launch queue timeout check.
- Baseline stealth on/off check.
- Explicit create/delete process-reaping check.
- Forceful client-disconnect cleanup check.

Run load tests:

```bash
cd tests/integration
EIGER_HOST=127.0.0.1:3102 npm run load
EIGER_HOST=127.0.0.1:3102 npm run load:30m
```

Run the RSS recycle regression with a deliberately low ceiling:

```bash
EIGER_BIND_ADDR=127.0.0.1:3103 \
EIGER_CHROME_NO_SANDBOX=false \
EIGER_MAINTENANCE_INTERVAL_SECS=1 \
EIGER_CDP_HEALTH_INTERVAL_SECS=2 \
EIGER_PER_SESSION_RSS_LIMIT_MB=900 \
cargo run -p eiger-server

cd tests/integration
EIGER_HOST=127.0.0.1:3103 EIGER_LEAK_MB=128 npm run rss-limit
```

Run Docker quickstart verification when a Docker daemon is available:

```bash
cd tests/integration
npm run docker:quickstart
npm run docker:shm-footgun
```
