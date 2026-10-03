import { createServer } from 'node:http';
import {
  generateKeyPairSync,
  createSign,
  createHash,
  randomBytes,
  timingSafeEqual,
} from 'node:crypto';
import { pathToFileURL } from 'node:url';

const DEFAULT_CLIENT_ID = 'kyro-e2e-client';
const DEFAULT_MODEL = 'synthetic-structured';
const MAX_PROVIDER_BODY_BYTES = 4 * 1024 * 1024;

function base64url(value) {
  return Buffer.from(value).toString('base64url');
}

function readJsonBody(req, maxBytes = MAX_PROVIDER_BODY_BYTES) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on('data', (chunk) => {
      size += chunk.length;
      if (size > maxBytes) {
        reject(Object.assign(new Error('request body exceeds test provider limit'), { statusCode: 413 }));
        req.destroy();
        return;
      }
      chunks.push(chunk);
    });
    req.on('end', () => {
      try {
        resolve(JSON.parse(Buffer.concat(chunks).toString('utf8')));
      } catch {
        reject(Object.assign(new Error('invalid JSON request'), { statusCode: 400 }));
      }
    });
    req.on('error', reject);
  });
}

function sendJson(res, statusCode, value) {
  const body = Buffer.from(JSON.stringify(value));
  res.writeHead(statusCode, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': body.length,
    'cache-control': 'no-store',
  });
  res.end(body);
}

function isLoopbackRedirect(value) {
  try {
    const url = new URL(value);
    return ['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname) &&
      ['http:', 'https:'].includes(url.protocol);
  } catch {
    return false;
  }
}

function signIdToken(privateKey, kid, claims) {
  const header = base64url(JSON.stringify({ alg: 'RS256', typ: 'JWT', kid }));
  const payload = base64url(JSON.stringify(claims));
  const input = `${header}.${payload}`;
  const signer = createSign('RSA-SHA256');
  signer.update(input);
  signer.end();
  return `${input}.${signer.sign(privateKey).toString('base64url')}`;
}

function makeCompletion(model, scenario) {
  if (scenario.responseJson !== undefined) {
    return {
      id: `chatcmpl-${randomBytes(8).toString('hex')}`,
      object: 'chat.completion',
      created: Math.floor(Date.now() / 1000),
      model,
      choices: [{
        index: 0,
        message: { role: 'assistant', content: JSON.stringify(scenario.responseJson) },
        finish_reason: 'stop',
      }],
      usage: { prompt_tokens: 17, completion_tokens: 11, total_tokens: 28 },
    };
  }

  const content = scenario.responseContent ?? JSON.stringify({
    schema_id: 'synthetic-structured-output',
    schema_version: '1',
    data: {
      summary: 'Synthetic project title: Cedar.',
      items: ['Cedar'],
    },
  });
  return {
    id: `chatcmpl-${randomBytes(8).toString('hex')}`,
    object: 'chat.completion',
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [{
      index: 0,
      message: { role: 'assistant', content },
      finish_reason: 'stop',
    }],
    usage: { prompt_tokens: 17, completion_tokens: 11, total_tokens: 28 },
  };
}

/**
 * Start one loopback-only mock for both static OIDC and OpenAI-compatible inference.
 * RSA material is generated in memory for each process and is never written to disk.
 */
export async function startSyntheticProvider({
  host = '127.0.0.1',
  port = 0,
  publicHost = host,
  controlHost = '127.0.0.1',
  controlPort = 0,
  clientId = DEFAULT_CLIENT_ID,
  defaultSubject = 'synthetic-user-1',
  controlToken,
  expectedApiKey = 'synthetic-e2e-model-key',
} = {}) {
  const { publicKey, privateKey } = generateKeyPairSync('rsa', {
    modulusLength: 2048,
    publicExponent: 0x10001,
  });
  const keyId = randomBytes(12).toString('hex');
  const publicJwk = publicKey.export({ format: 'jwk' });
  const oidcJwk = { ...publicJwk, use: 'sig', alg: 'RS256', kid: keyId };

  let origin;
  let nextIdentity = { sub: defaultSubject, email: `${defaultSubject}@example.invalid` };
  let nextOidcClaimOverrides = {};
  const authorizationCodes = new Map();
  let activeInferenceScenario = { mode: 'success' };
  const heldResponses = new Set();
  const counters = {
    authorizationRequests: 0,
    tokenRequests: 0,
    jwksRequests: 0,
    inferenceRequests: 0,
    inferenceFailures: 0,
    inferenceAborted: 0,
    inferenceCompleted: 0,
    inferenceBytesReturned: 0,
    inferenceStatuses: {},
    apiKeyAcceptedRequests: 0,
    apiKeyRejectedRequests: 0,
    lastModel: null,
    lastStructuredFormat: null,
    lastRequestBodyBytes: 0,
  };
  const setNextIdentity = (sub, email = `${sub}@example.invalid`) => {
    if (!/^synthetic-user-[a-z0-9-]+$/.test(sub)) throw new Error('synthetic subject must use the synthetic-user-* namespace');
    nextIdentity = { sub, email };
  };
  const setInferenceScenario = (scenario = {}) => {
    const allowed = new Set([
      'mode', 'failuresRemaining', 'statusCode', 'responseBytes', 'delayMs',
      'responseJson', 'responseContent',
    ]);
    if (!scenario || typeof scenario !== 'object' || Array.isArray(scenario) ||
        Object.keys(scenario).some((key) => !allowed.has(key))) {
      throw new Error('invalid synthetic inference scenario');
    }
    const mode = scenario.mode ?? 'success';
    if (!['success', 'status', 'malformed', 'oversized', 'hold'].includes(mode)) {
      throw new Error(`unsupported synthetic inference mode: ${mode}`);
    }
    if ((scenario.failuresRemaining !== undefined &&
         (!Number.isInteger(scenario.failuresRemaining) || scenario.failuresRemaining < 0 || scenario.failuresRemaining > 20)) ||
        (scenario.statusCode !== undefined &&
         (!Number.isInteger(scenario.statusCode) || scenario.statusCode < 400 || scenario.statusCode > 599)) ||
        (scenario.responseBytes !== undefined &&
         (!Number.isInteger(scenario.responseBytes) || scenario.responseBytes < 1 || scenario.responseBytes > 8 * 1024 * 1024)) ||
        (scenario.delayMs !== undefined &&
         (!Number.isInteger(scenario.delayMs) || scenario.delayMs < 0 || scenario.delayMs > 30_000)) ||
        (scenario.responseContent !== undefined &&
         (typeof scenario.responseContent !== 'string' || scenario.responseContent.length > 8 * 1024 * 1024)) ||
        (scenario.responseJson !== undefined &&
         (scenario.responseJson === null || typeof scenario.responseJson !== 'object' || Array.isArray(scenario.responseJson)))) {
      throw new Error('synthetic inference scenario parameter is outside its bounds');
    }
    activeInferenceScenario = { ...scenario, mode };
  };
  const releaseInference = () => {
    for (const release of heldResponses) release();
    heldResponses.clear();
  };
  const snapshot = () => structuredClone(counters);

  const server = createServer(async (req, res) => {
    const requestUrl = new URL(req.url ?? '/', origin);

    try {
      if (req.method === 'GET' && requestUrl.pathname === '/oidc/authorize') {
        counters.authorizationRequests += 1;
        const params = requestUrl.searchParams;
        const challenge = params.get('code_challenge') ?? '';
        const state = params.get('state') ?? '';
        const nonce = params.get('nonce') ?? '';
        const redirectUri = params.get('redirect_uri') ?? '';
        const allowed = params.get('client_id') === clientId &&
          params.get('response_type') === 'code' &&
          (params.get('scope') ?? '').split(/\s+/).includes('openid') &&
          params.get('code_challenge_method') === 'S256' &&
          /^[A-Za-z0-9_-]{43}$/.test(challenge) && state.length >= 16 && nonce.length >= 16 &&
          isLoopbackRedirect(redirectUri);
        if (!allowed) {
          sendJson(res, 400, { error: 'invalid_synthetic_authorization_request' });
          return;
        }

        const code = randomBytes(24).toString('base64url');
        authorizationCodes.set(code, {
          challenge,
          nonce,
          redirectUri,
          sub: nextIdentity.sub,
          email: nextIdentity.email,
          claimOverrides: nextOidcClaimOverrides,
          clientId,
          createdAt: Date.now(),
        });
        nextIdentity = { sub: defaultSubject, email: `${defaultSubject}@example.invalid` };
        nextOidcClaimOverrides = {};
        sendJson(res, 200, { code, state });
        return;
      }

      if (req.method === 'POST' && requestUrl.pathname === '/oidc/token') {
        counters.tokenRequests += 1;
        const chunks = [];
        for await (const chunk of req) chunks.push(chunk);
        const form = new URLSearchParams(Buffer.concat(chunks).toString('utf8'));
        const code = form.get('code') ?? '';
        const entry = authorizationCodes.get(code);
        const verifier = form.get('code_verifier') ?? '';
        const expectedChallenge = base64url(createHash('sha256').update(verifier).digest());
        if (form.get('grant_type') !== 'authorization_code' || !entry ||
            entry.createdAt + 60_000 < Date.now() || form.get('client_id') !== clientId ||
            form.get('redirect_uri') !== entry.redirectUri || expectedChallenge !== entry.challenge) {
          sendJson(res, 400, { error: 'invalid_synthetic_token_request' });
          return;
        }
        authorizationCodes.delete(code);
        const now = Math.floor(Date.now() / 1000);
        const idToken = signIdToken(privateKey, keyId, {
          iss: `${origin}/issuer`,
          sub: entry.sub,
          aud: entry.clientId,
          iat: now,
          exp: now + 300,
          nonce: entry.nonce,
          email: entry.email,
          email_verified: true,
          ...entry.claimOverrides,
        });
        sendJson(res, 200, {
          access_token: randomBytes(24).toString('base64url'),
          token_type: 'Bearer',
          expires_in: 300,
          id_token: idToken,
        });
        return;
      }

      if (req.method === 'GET' && requestUrl.pathname === '/oidc/jwks') {
        counters.jwksRequests += 1;
        sendJson(res, 200, { keys: [oidcJwk] });
        return;
      }

      if (req.method === 'GET' && requestUrl.pathname === '/.well-known/openid-configuration') {
        sendJson(res, 200, {
          issuer: `${origin}/issuer`,
          authorization_endpoint: `${origin}/oidc/authorize`,
          token_endpoint: `${origin}/oidc/token`,
          jwks_uri: `${origin}/oidc/jwks`,
          response_types_supported: ['code'],
          subject_types_supported: ['public'],
          id_token_signing_alg_values_supported: ['RS256'],
          code_challenge_methods_supported: ['S256'],
        });
        return;
      }

      if (req.method === 'POST' && requestUrl.pathname === '/v1/chat/completions') {
        counters.inferenceRequests += 1;
        const suppliedApiKey = req.headers.authorization ?? '';
        const expectedAuthorization = `Bearer ${expectedApiKey}`;
        const suppliedBytes = Buffer.from(suppliedApiKey);
        const expectedBytes = Buffer.from(expectedAuthorization);
        if (suppliedBytes.length !== expectedBytes.length ||
            !timingSafeEqual(suppliedBytes, expectedBytes)) {
          counters.apiKeyRejectedRequests += 1;
          counters.inferenceStatuses[401] = (counters.inferenceStatuses[401] ?? 0) + 1;
          sendJson(res, 401, { error: { message: 'synthetic authentication failed' } });
          return;
        }
        counters.apiKeyAcceptedRequests += 1;
        const bodyBytes = Number(req.headers['content-length'] ?? 0);
        counters.lastRequestBodyBytes = bodyBytes;
        const body = await readJsonBody(req);
        const scenario = { ...activeInferenceScenario };
        const model = typeof body.model === 'string' ? body.model : DEFAULT_MODEL;
        counters.lastModel = model;
        counters.lastStructuredFormat = body.response_format?.type ?? null;

        let aborted = false;
        let releaseThisCall;
        res.on('close', () => {
          if (!res.writableEnded && !aborted) {
            aborted = true;
            counters.inferenceAborted += 1;
            releaseThisCall?.();
          }
        });

        if (scenario.mode === 'hold') {
          await new Promise((resolve) => {
            releaseThisCall = () => {
              heldResponses.delete(releaseThisCall);
              resolve();
            };
            heldResponses.add(releaseThisCall);
            if (scenario.delayMs > 0) setTimeout(releaseThisCall, scenario.delayMs);
          });
        } else if (scenario.delayMs > 0) {
          await new Promise((resolve) => setTimeout(resolve, scenario.delayMs));
        }
        if (aborted || res.destroyed) return;

        if (scenario.mode === 'status' && (scenario.failuresRemaining ?? 1) > 0) {
          activeInferenceScenario = {
            ...scenario,
            failuresRemaining: Math.max(0, (scenario.failuresRemaining ?? 1) - 1),
          };
          counters.inferenceFailures += 1;
          const statusCode = Number.isInteger(scenario.statusCode) ? scenario.statusCode : 503;
          counters.inferenceStatuses[statusCode] = (counters.inferenceStatuses[statusCode] ?? 0) + 1;
          sendJson(res, statusCode, { error: { message: 'synthetic provider failure', type: 'server_error' } });
          counters.inferenceBytesReturned += Buffer.byteLength('{"error":{"message":"synthetic provider failure","type":"server_error"}}');
          counters.inferenceCompleted += 1;
          return;
        }

        if (scenario.mode === 'malformed') {
          const bodyBuffer = Buffer.from('{"choices":');
          counters.inferenceStatuses[200] = (counters.inferenceStatuses[200] ?? 0) + 1;
          counters.inferenceBytesReturned += bodyBuffer.length;
          counters.inferenceCompleted += 1;
          res.writeHead(200, { 'content-type': 'application/json', 'content-length': bodyBuffer.length });
          res.end(bodyBuffer);
          return;
        }

        if (scenario.mode === 'oversized') {
          const size = Math.max(1, Math.min(Number(scenario.responseBytes ?? 2 * 1024 * 1024), 8 * 1024 * 1024));
          const payload = Buffer.from(JSON.stringify(makeCompletion(model, {
            responseContent: 'x'.repeat(size),
          })));
          counters.inferenceStatuses[200] = (counters.inferenceStatuses[200] ?? 0) + 1;
          counters.inferenceBytesReturned += payload.length;
          counters.inferenceCompleted += 1;
          res.writeHead(200, { 'content-type': 'application/json', 'content-length': payload.length });
          res.end(payload);
          return;
        }

        const response = Buffer.from(JSON.stringify(makeCompletion(model, scenario)));
        counters.inferenceStatuses[200] = (counters.inferenceStatuses[200] ?? 0) + 1;
        counters.inferenceBytesReturned += response.length;
        counters.inferenceCompleted += 1;
        res.writeHead(200, {
          'content-type': 'application/json; charset=utf-8',
          'content-length': response.length,
          'x-request-id': `synthetic-${randomBytes(8).toString('hex')}`,
        });
        res.end(response);
        return;
      }

      sendJson(res, 404, { error: 'not_found' });
    } catch (error) {
      if (!res.headersSent && !res.destroyed) {
        sendJson(res, Number.isInteger(error.statusCode) ? error.statusCode : 500, {
          error: 'synthetic_provider_error',
        });
      } else if (!res.destroyed) {
        res.destroy();
      }
    }
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, host, resolve);
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('synthetic provider did not bind a TCP port');
  origin = `http://${publicHost}:${address.port}`;

  let controlServer;
  let controlOrigin;
  if (controlToken !== undefined) {
    if (typeof controlToken !== 'string' || controlToken.length < 32) {
      throw new Error('synthetic control token must contain at least 32 characters');
    }
    const expectedControlBytes = Buffer.from(`Bearer ${controlToken}`);
    controlServer = createServer(async (req, res) => {
      const suppliedBytes = Buffer.from(req.headers.authorization ?? '');
      if (suppliedBytes.length !== expectedControlBytes.length ||
          !timingSafeEqual(suppliedBytes, expectedControlBytes)) {
        sendJson(res, 401, { error: 'synthetic_control_unauthorized' });
        return;
      }

      const controlUrl = new URL(req.url ?? '/', 'http://synthetic-control.invalid');
      try {
        if (req.method === 'GET' && controlUrl.pathname === '/__e2e/ready') {
          sendJson(res, 200, { ready: true });
          return;
        }
        if (req.method === 'GET' && controlUrl.pathname === '/__e2e/snapshot') {
          sendJson(res, 200, snapshot());
          return;
        }
        if (req.method === 'POST' && controlUrl.pathname === '/__e2e/identity') {
          const body = await readJsonBody(req, 16 * 1024);
          const { sub, email, claims = {} } = body;
          if (typeof sub !== 'string' || !/^synthetic-user-[a-z0-9-]+$/.test(sub) ||
              (email !== undefined && (typeof email !== 'string' || email.length > 254)) ||
              !claims || typeof claims !== 'object' || Array.isArray(claims)) {
            sendJson(res, 400, { error: 'invalid_synthetic_identity' });
            return;
          }
          const allowedClaimOverrides = new Set(['iss', 'aud', 'nonce', 'exp', 'nbf', 'iat']);
          if (Object.keys(claims).some((key) => !allowedClaimOverrides.has(key)) ||
              Object.entries(claims).some(([key, value]) =>
                (['exp', 'nbf', 'iat'].includes(key) && !Number.isInteger(value)) ||
                (!['exp', 'nbf', 'iat'].includes(key) && typeof value !== 'string'))) {
            sendJson(res, 400, { error: 'invalid_synthetic_claim_override' });
            return;
          }
          setNextIdentity(sub, email ?? `${sub}@example.invalid`);
          nextOidcClaimOverrides = { ...claims };
          sendJson(res, 200, { configured: true });
          return;
        }
        if (req.method === 'POST' && controlUrl.pathname === '/__e2e/scenario') {
          const body = await readJsonBody(req, 16 * 1024);
          if (!body || typeof body !== 'object' || Array.isArray(body) ||
              Object.keys(body).some((key) => key !== 'inference')) {
            sendJson(res, 400, { error: 'invalid_synthetic_scenario' });
            return;
          }
          if (body.inference !== undefined) setInferenceScenario(body.inference);
          sendJson(res, 200, { configured: true });
          return;
        }
        if (req.method === 'POST' && controlUrl.pathname === '/__e2e/release') {
          const released = heldResponses.size;
          releaseInference();
          sendJson(res, 200, { released });
          return;
        }
        sendJson(res, 404, { error: 'not_found' });
      } catch (error) {
        sendJson(res, Number.isInteger(error.statusCode) ? error.statusCode : 400, {
          error: 'invalid_synthetic_control_request',
        });
      }
    });
    await new Promise((resolve, reject) => {
      controlServer.once('error', reject);
      controlServer.listen(controlPort, controlHost, resolve);
    });
    const controlAddress = controlServer.address();
    if (!controlAddress || typeof controlAddress === 'string') {
      throw new Error('synthetic control plane did not bind a TCP port');
    }
    controlOrigin = `http://${publicHost}:${controlAddress.port}`;
  }

  return {
    origin,
    issuer: `${origin}/issuer`,
    authorizationEndpoint: `${origin}/oidc/authorize`,
    tokenEndpoint: `${origin}/oidc/token`,
    jwksUri: `${origin}/oidc/jwks`,
    inferenceBaseUrl: `${origin}/v1`,
    controlOrigin,
    clientId,
    model: DEFAULT_MODEL,
    setNextIdentity,
    setInferenceScenario,
    releaseInference,
    snapshot,
    async close() {
      releaseInference();
      const servers = [controlServer, server].filter(Boolean);
      await Promise.all(servers.map((instance) => new Promise((resolve, reject) => {
        instance.close((error) => error ? reject(error) : resolve());
        instance.closeAllConnections?.();
      })));
    },
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const provider = await startSyntheticProvider({
    host: process.env.KYRO_E2E_PROVIDER_HOST ?? '127.0.0.1',
    publicHost: process.env.KYRO_E2E_PROVIDER_PUBLIC_HOST ?? '127.0.0.1',
    port: Number(process.env.KYRO_E2E_PROVIDER_PORT ?? 9090),
    controlHost: process.env.KYRO_E2E_CONTROL_HOST ?? '0.0.0.0',
    controlPort: Number(process.env.KYRO_E2E_CONTROL_PORT ?? 9091),
    controlToken: process.env.KYRO_E2E_CONTROL_TOKEN,
    expectedApiKey: process.env.KYRO_MODEL_API_KEY ?? 'synthetic-e2e-model-key',
  });
  process.stdout.write(`${JSON.stringify({
    status: 'ready',
    host: process.env.KYRO_E2E_PROVIDER_PUBLIC_HOST ?? '127.0.0.1',
    endpoints: {
      issuer: provider.issuer,
      authorization: provider.authorizationEndpoint,
      token: provider.tokenEndpoint,
      jwks: provider.jwksUri,
      inference: provider.inferenceBaseUrl,
    },
    client_id: provider.clientId,
    model: provider.model,
    control_origin: provider.controlOrigin,
  })}\n`);
  const stop = async () => {
    await provider.close();
    process.exit(0);
  };
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
}
