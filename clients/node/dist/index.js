export class EigerError extends Error {
    status;
    body;
    constructor(message, status, body) {
        super(message);
        this.name = 'EigerError';
        this.status = status;
        this.body = body;
    }
}
export class EigerClient {
    baseUrl;
    token;
    fetchImpl;
    constructor(options = {}) {
        this.baseUrl = new URL(options.baseUrl ?? 'http://127.0.0.1:3000');
        this.token = options.token;
        this.fetchImpl = options.fetch ?? globalThis.fetch;
        if (!this.fetchImpl) {
            throw new Error('fetch is not available; use Node 18 or pass a fetch implementation');
        }
    }
    async createSession(options = {}) {
        return this.requestJson('/sessions', {
            method: 'POST',
            body: JSON.stringify(options),
        });
    }
    async getSession(id) {
        return this.requestJson(`/sessions/${encodeURIComponent(id)}`);
    }
    async listSessions() {
        return this.requestJson('/sessions');
    }
    async deleteSession(id) {
        await this.request(`/sessions/${encodeURIComponent(id)}`, { method: 'DELETE' });
    }
    async connect(options = {}) {
        const session = await this.createSession(options);
        return session.cdpWsUrl;
    }
    sessionWebSocketUrl(id) {
        const url = this.url(`/sessions/${encodeURIComponent(id)}/cdp`);
        url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
        if (this.token) {
            url.searchParams.set('token', this.token);
        }
        return url.toString();
    }
    async scrape(request) {
        return this.requestJson('/scrape', {
            method: 'POST',
            body: JSON.stringify(request),
        });
    }
    async screenshot(request) {
        return this.requestBinary('/screenshot', request);
    }
    async pdf(request) {
        return this.requestBinary('/pdf', request);
    }
    async requestJson(path, init = {}) {
        const response = await this.request(path, init);
        return response.json();
    }
    async requestBinary(path, body) {
        const response = await this.request(path, {
            method: 'POST',
            body: JSON.stringify(body),
        });
        return {
            data: await response.arrayBuffer(),
            contentType: response.headers.get('content-type') ?? 'application/octet-stream',
        };
    }
    async request(path, init = {}) {
        const headers = new Headers(init.headers);
        if (init.body && !headers.has('content-type')) {
            headers.set('content-type', 'application/json');
        }
        if (this.token && !headers.has('authorization')) {
            headers.set('authorization', `Bearer ${this.token}`);
        }
        const response = await this.fetchImpl(this.url(path), {
            ...init,
            headers,
        });
        if (!response.ok) {
            const body = await response.text();
            throw new EigerError(`Eiger request failed: ${response.status}`, response.status, body);
        }
        return response;
    }
    url(path) {
        return new URL(path, this.baseUrl);
    }
}
export default EigerClient;
