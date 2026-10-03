import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { startSyntheticProvider } from '../../scripts/synthetic-provider.mjs';

const apiKey = `synthetic-${randomBytes(32).toString('hex')}`;
const controlToken = randomBytes(32).toString('base64url');
const provider = await startSyntheticProvider({
  host: '0.0.0.0',
  publicHost: '127.0.0.1',
  controlHost: '127.0.0.1',
  port: 0,
  controlPort: 0,
  controlToken,
  expectedApiKey: apiKey,
});

const controlHeaders = {
  authorization: `Bearer ${controlToken}`,
  'content-type': 'application/json',
};

try {
  const unauthorized = await fetch(`${provider.controlOrigin}/__e2e/snapshot`);
  assert.equal(unauthorized.status, 401);

  const ready = await fetch(`${provider.controlOrigin}/__e2e/ready`, {
    headers: { authorization: `Bearer ${controlToken}` },
  });
  assert.equal(ready.status, 200);
  assert.deepEqual(await ready.json(), { ready: true });

  const identity = await fetch(`${provider.controlOrigin}/__e2e/identity`, {
    method: 'POST',
    headers: controlHeaders,
    body: JSON.stringify({
      sub: 'synthetic-user-invalid-audience',
      claims: { aud: 'another-synthetic-client' },
    }),
  });
  assert.equal(identity.status, 200);

  const verifier = randomBytes(32).toString('base64url');
  const challenge = createHash('sha256').update(verifier).digest('base64url');
  const authorizationUrl = new URL(provider.authorizationEndpoint);
  authorizationUrl.search = new URLSearchParams({
    response_type: 'code',
    scope: 'openid',
    client_id: provider.clientId,
    redirect_uri: 'http://127.0.0.1:43210/v1/auth/callback',
    state: randomBytes(24).toString('base64url'),
    nonce: randomBytes(24).toString('base64url'),
    code_challenge: challenge,
    code_challenge_method: 'S256',
  }).toString();
  const authorizationResponse = await fetch(authorizationUrl);
  assert.equal(authorizationResponse.status, 200);
  const authorization = await authorizationResponse.json();
  const tokenResponse = await fetch(provider.tokenEndpoint, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      grant_type: 'authorization_code',
      code: authorization.code,
      redirect_uri: 'http://127.0.0.1:43210/v1/auth/callback',
      client_id: provider.clientId,
      code_verifier: verifier,
    }),
  });
  assert.equal(tokenResponse.status, 200);
  const idToken = (await tokenResponse.json()).id_token;
  const claims = JSON.parse(Buffer.from(idToken.split('.')[1], 'base64url'));
  assert.equal(claims.aud, 'another-synthetic-client');
  assert.equal(claims.sub, 'synthetic-user-invalid-audience');

  const inferenceUrl = `${provider.inferenceBaseUrl}/chat/completions`;
  const body = JSON.stringify({ model: provider.model, messages: [] });
  const rejectedInference = await fetch(inferenceUrl, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: 'Bearer wrong-synthetic-key' },
    body,
  });
  assert.equal(rejectedInference.status, 401);
  const acceptedInference = await fetch(inferenceUrl, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${apiKey}` },
    body,
  });
  assert.equal(acceptedInference.status, 200);

  const snapshotResponse = await fetch(`${provider.controlOrigin}/__e2e/snapshot`, {
    headers: { authorization: `Bearer ${controlToken}` },
  });
  assert.equal(snapshotResponse.status, 200);
  const snapshot = await snapshotResponse.json();
  assert.equal(snapshot.apiKeyAcceptedRequests, 1);
  assert.equal(snapshot.apiKeyRejectedRequests, 1);
  assert.equal(snapshot.inferenceRequests, 2);

  process.stdout.write(`${JSON.stringify({
    result: 'PASS',
    scope: 'synthetic control plane and model-key gate only',
    unauthorizedControlRejected: true,
    invalidAudienceScenarioIssued: true,
    inferenceKeyAccepted: snapshot.apiKeyAcceptedRequests,
    inferenceKeyRejected: snapshot.apiKeyRejectedRequests,
    inferenceRequests: snapshot.inferenceRequests,
    credentials_recorded: false,
  })}\n`);
} finally {
  await provider.close();
}
