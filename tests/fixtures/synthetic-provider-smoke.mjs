import assert from 'node:assert/strict';
import {
  createHash,
  createPublicKey,
  randomBytes,
  verify,
} from 'node:crypto';
import { startSyntheticProvider } from '../../scripts/synthetic-provider.mjs';

const provider = await startSyntheticProvider();
const modelHeaders = {
  'content-type': 'application/json',
  authorization: 'Bearer synthetic-e2e-model-key',
};

try {
  const verifier = randomBytes(32).toString('base64url');
  const challenge = createHash('sha256').update(verifier).digest('base64url');
  const nonce = randomBytes(24).toString('base64url');
  const state = randomBytes(24).toString('base64url');
  const redirectUri = 'http://127.0.0.1:43210/v1/auth/callback';
  const authorizationUrl = new URL(provider.authorizationEndpoint);
  authorizationUrl.search = new URLSearchParams({
    response_type: 'code',
    scope: 'openid',
    client_id: provider.clientId,
    redirect_uri: redirectUri,
    state,
    nonce,
    code_challenge: challenge,
    code_challenge_method: 'S256',
  }).toString();

  const authorizationResponse = await fetch(authorizationUrl);
  assert.equal(authorizationResponse.status, 200);
  const authorization = await authorizationResponse.json();
  assert.equal(authorization.state, state);

  const tokenResponse = await fetch(provider.tokenEndpoint, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      grant_type: 'authorization_code',
      code: authorization.code,
      redirect_uri: redirectUri,
      client_id: provider.clientId,
      code_verifier: verifier,
    }),
  });
  assert.equal(tokenResponse.status, 200);
  const { id_token: idToken } = await tokenResponse.json();
  const [headerPart, claimPart, signaturePart] = idToken.split('.');
  const jwksResponse = await fetch(provider.jwksUri);
  assert.equal(jwksResponse.status, 200);
  const jwks = await jwksResponse.json();
  const publicKey = createPublicKey({ key: jwks.keys[0], format: 'jwk' });
  assert.equal(
    verify('RSA-SHA256', Buffer.from(`${headerPart}.${claimPart}`), publicKey, Buffer.from(signaturePart, 'base64url')),
    true,
  );
  const claims = JSON.parse(Buffer.from(claimPart, 'base64url'));
  assert.equal(claims.iss, provider.issuer);
  assert.equal(claims.aud, provider.clientId);
  assert.equal(claims.nonce, nonce);
  assert.equal(claims.sub, 'synthetic-user-1');

  const invalidVerifier = randomBytes(32).toString('base64url');
  const invalidChallenge = createHash('sha256').update(randomBytes(32)).digest('base64url');
  const invalidAuthorizationUrl = new URL(provider.authorizationEndpoint);
  invalidAuthorizationUrl.search = new URLSearchParams({
    response_type: 'code',
    scope: 'openid',
    client_id: provider.clientId,
    redirect_uri: redirectUri,
    state: randomBytes(24).toString('base64url'),
    nonce: randomBytes(24).toString('base64url'),
    code_challenge: invalidChallenge,
    code_challenge_method: 'S256',
  }).toString();
  const invalidAuthorizationResponse = await fetch(invalidAuthorizationUrl);
  assert.equal(invalidAuthorizationResponse.status, 200);
  const invalidAuthorization = await invalidAuthorizationResponse.json();
  const invalidToken = await fetch(provider.tokenEndpoint, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      grant_type: 'authorization_code',
      code: invalidAuthorization.code,
      redirect_uri: redirectUri,
      client_id: provider.clientId,
      code_verifier: invalidVerifier,
    }),
  });
  assert.equal(invalidToken.status, 400);

  const completionResponse = await fetch(`${provider.inferenceBaseUrl}/chat/completions`, {
    method: 'POST',
    headers: modelHeaders,
    body: JSON.stringify({
      model: provider.model,
      messages: [{ role: 'user', content: 'synthetic input' }],
      response_format: { type: 'json_object' },
    }),
  });
  assert.equal(completionResponse.status, 200);
  const completion = await completionResponse.json();
  const structuredOutput = JSON.parse(completion.choices[0].message.content);
  assert.equal(structuredOutput.schema_id, 'synthetic-structured-output');
  assert.equal(structuredOutput.schema_version, '1');
  assert.deepEqual(structuredOutput.data, {
    summary: 'Synthetic project title: Cedar.',
    items: ['Cedar'],
  });

  provider.setInferenceScenario({ mode: 'status', failuresRemaining: 1, statusCode: 503 });
  const failedResponse = await fetch(`${provider.inferenceBaseUrl}/chat/completions`, {
    method: 'POST',
    headers: modelHeaders,
    body: JSON.stringify({ model: provider.model, messages: [] }),
  });
  const retryResponse = await fetch(`${provider.inferenceBaseUrl}/chat/completions`, {
    method: 'POST',
    headers: modelHeaders,
    body: JSON.stringify({ model: provider.model, messages: [] }),
  });
  assert.equal(failedResponse.status, 503);
  assert.equal(retryResponse.status, 200);

  provider.setInferenceScenario({ mode: 'malformed' });
  const malformedResponse = await fetch(`${provider.inferenceBaseUrl}/chat/completions`, {
    method: 'POST',
    headers: modelHeaders,
    body: JSON.stringify({ model: provider.model, messages: [] }),
  });
  assert.equal(malformedResponse.status, 200);
  await assert.rejects(() => malformedResponse.json(), SyntaxError);

  provider.setInferenceScenario({ mode: 'oversized', responseBytes: 64 * 1024 });
  const oversizedResponse = await fetch(`${provider.inferenceBaseUrl}/chat/completions`, {
    method: 'POST',
    headers: modelHeaders,
    body: JSON.stringify({ model: provider.model, messages: [] }),
  });
  assert.equal(oversizedResponse.status, 200);
  assert.ok(Number(oversizedResponse.headers.get('content-length')) >= 64 * 1024);
  const oversizedCompletion = await oversizedResponse.json();
  assert.ok(oversizedCompletion.choices[0].message.content.length >= 64 * 1024);

  provider.setInferenceScenario({ mode: 'hold' });
  const controller = new AbortController();
  const heldRequest = fetch(`${provider.inferenceBaseUrl}/chat/completions`, {
    method: 'POST',
    headers: modelHeaders,
    body: JSON.stringify({ model: provider.model, messages: [] }),
    signal: controller.signal,
  });
  await new Promise((resolve) => setTimeout(resolve, 25));
  controller.abort();
  await assert.rejects(heldRequest, (error) => error.name === 'AbortError');
  await new Promise((resolve) => setTimeout(resolve, 10));

  const counts = provider.snapshot();
  assert.equal(counts.authorizationRequests, 2);
  assert.equal(counts.tokenRequests, 2);
  assert.equal(counts.jwksRequests, 1);
  assert.equal(counts.inferenceRequests, 6);
  assert.equal(counts.inferenceFailures, 1);
  assert.equal(counts.inferenceAborted, 1);
  process.stdout.write(`${JSON.stringify({
    result: 'PASS',
    oidc: 'valid PKCE exchange and invalid-verifier rejection; exact issuer/audience/nonce and RS256 signature verified',
    inference: 'success, one-shot 503, malformed JSON, oversized response and aborted hold observed',
    inferenceRequests: counts.inferenceRequests,
    inferenceFailures: counts.inferenceFailures,
    inferenceAborted: counts.inferenceAborted,
    statuses: counts.inferenceStatuses,
  })}\n`);
} finally {
  await provider.close();
}
