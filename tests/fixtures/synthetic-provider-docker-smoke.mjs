import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const suffix = randomBytes(6).toString('hex');
const image = `kyro-p1-provider-smoke:${suffix}`;
const container = `kyro-p1-provider-smoke-${suffix}`;
const apiKey = `synthetic-${randomBytes(32).toString('hex')}`;
const controlToken = randomBytes(32).toString('base64url');
const providerPort = 9090;
const controlPort = 9091;

function docker(args, { allowFailure = false, timeout = 60_000 } = {}) {
  const result = spawnSync('docker', args, {
    cwd: root,
    encoding: 'utf8',
    windowsHide: true,
    timeout,
    maxBuffer: 2 * 1024 * 1024,
  });
  if (result.error) throw new Error(`docker could not run (${result.error.code ?? 'unknown'})`);
  if (!allowFailure && result.status !== 0) throw new Error(`docker exited with status ${result.status ?? 'unknown'}`);
  return result;
}

async function isPortAvailable(port) {
  return new Promise((resolveAvailable, reject) => {
    const server = createServer();
    server.once('error', (error) => {
      if (error.code === 'EADDRINUSE') resolveAvailable(false);
      else reject(error);
    });
    server.listen(port, '127.0.0.1', () => server.close(() => resolveAvailable(true)));
  });
}

async function waitForProvider() {
  const controlUrl = `http://127.0.0.1:${controlPort}/__e2e/ready`;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const response = await fetch(controlUrl, {
        headers: { authorization: `Bearer ${controlToken}` },
        signal: AbortSignal.timeout(1000),
      });
      if (response.status === 200) return;
    } catch {
      // The process is still starting; retain only the final timeout signal.
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 200));
  }
  throw new Error('synthetic provider container did not become ready');
}

async function main() {
  for (const port of [providerPort, controlPort]) {
    if (!await isPortAvailable(port)) throw new Error(`required loopback port ${port} is occupied`);
  }

  let containerStarted = false;
  let imageBuilt = false;
  try {
    docker([
      'build', '-f', 'tests/fixtures/Dockerfile.synthetic-provider',
      '-t', image, '.',
    ], { timeout: 180_000 });
    imageBuilt = true;
    docker([
      'run', '--detach', '--name', container,
      '--publish', `127.0.0.1:${providerPort}:${providerPort}`,
      '--publish', `127.0.0.1:${controlPort}:${controlPort}`,
      '--env', `KYRO_E2E_CONTROL_TOKEN=${controlToken}`,
      '--env', `KYRO_MODEL_API_KEY=${apiKey}`,
      image,
    ]);
    containerStarted = true;
    await waitForProvider();

    const deniedControl = await fetch(`http://127.0.0.1:${controlPort}/__e2e/snapshot`);
    assert.equal(deniedControl.status, 401);

    const completion = await fetch(`http://127.0.0.1:${providerPort}/v1/chat/completions`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        authorization: `Bearer ${apiKey}`,
      },
      body: JSON.stringify({
        model: 'synthetic-structured',
        messages: [{ role: 'user', content: JSON.stringify({ content: 'Synthetic project title: Cedar.' }) }],
        response_format: { type: 'json_schema', json_schema: { name: 'synthetic-structured-output', strict: true } },
      }),
    });
    assert.equal(completion.status, 200);
    const data = await completion.json();
    const structured = JSON.parse(data.choices[0].message.content);
    assert.equal(structured.schema_id, 'synthetic-structured-output');
    assert.equal(structured.schema_version, '1');
    assert.deepEqual(structured.data, {
      summary: 'Synthetic project title: Cedar.',
      items: ['Cedar'],
    });

    const wrongKey = await fetch(`http://127.0.0.1:${providerPort}/v1/chat/completions`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        authorization: 'Bearer wrong-synthetic-key',
      },
      body: JSON.stringify({ model: 'synthetic-structured', messages: [] }),
    });
    assert.equal(wrongKey.status, 401);

    const snapshotResponse = await fetch(`http://127.0.0.1:${controlPort}/__e2e/snapshot`, {
      headers: { authorization: `Bearer ${controlToken}` },
    });
    assert.equal(snapshotResponse.status, 200);
    const snapshot = await snapshotResponse.json();
    assert.equal(snapshot.inferenceRequests, 2);
    assert.equal(snapshot.apiKeyAcceptedRequests, 1);
    assert.equal(snapshot.apiKeyRejectedRequests, 1);

    process.stdout.write(`${JSON.stringify({
      result: 'PASS',
      scope: 'Node provider container, loopback publishing, authenticated control and inference only',
      provider_requests: snapshot.inferenceRequests,
      model_keys_accepted: snapshot.apiKeyAcceptedRequests,
      model_keys_rejected: snapshot.apiKeyRejectedRequests,
      credentials_recorded: false,
    })}\n`);
  } finally {
    if (containerStarted) docker(['rm', '--force', container], { allowFailure: true });
    if (imageBuilt) docker(['image', 'rm', image], { allowFailure: true });
  }
}

main().catch((error) => {
  process.stderr.write(`synthetic-provider-docker-smoke: ${error.message}\n`);
  process.exitCode = 1;
});
