// This key is intentionally public and belongs exclusively to the synthetic
// OIDC test server. Never use it as an installation or provider credential.
import { generateKeyPairSync } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';
const { privateKey, publicKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
const output = new URL('../../../crates/kyro-app/tests/fixtures/oidc-public-test-key.json', import.meta.url);
mkdirSync(new URL('.', output), { recursive: true });
writeFileSync(output, JSON.stringify({
  notice: 'PUBLIC SYNTHETIC OIDC TEST KEY — NOT AN INSTALLATION CREDENTIAL',
  private_der: privateKey.export({ type: 'pkcs1', format: 'der' }).toString('base64'),
  jwk: { ...publicKey.export({ format: 'jwk' }), kid: 'synthetic-p2-oidc', use: 'sig', alg: 'RS256' },
}, null, 2) + '\n');
