import assert from 'node:assert/strict';
import { request } from 'node:http';
const base = 'http://127.0.0.1:5174/__kyro_native/';
const headers = { Origin: 'http://127.0.0.1:5174', 'X-Kyro-Local': '1', 'Content-Type': 'application/json' };
const scenarios = [
  ['unknown grant', 'select', headers, '["unknown"]', 200, 'Choisissez'],
  ['foreign origin', 'select', { ...headers, Origin: 'https://example.com' }, '[]', 403, 'refusé'],
  ['absent origin', 'select', { 'X-Kyro-Local': '1', 'Content-Type': 'application/json' }, '[]', 403, 'refusé'],
  ['foreign host', 'select', { ...headers, Host: 'example.com:5174' }, '[]', 403, ''],
  ['simple form', 'select', { ...headers, 'X-Kyro-Local': '', 'Content-Type': 'text/plain' }, '[]', 403, 'refusé'],
  ['oversized body', 'select', headers, ' '.repeat(4097), 413, ''],
  ['unknown operation', 'execute', headers, '[]', 404, ''],
  ['path traversal', 'prepareProject', headers, '["../escape"]', 200, 'nom de dossier'],
];
for (const [name, action, requestHeaders, body, status, message] of scenarios) {
  // Raw HTTP preserves the Host override; fetch may replace that forbidden header.
  const response = await new Promise((resolve, reject) => {
    const req = request(base + action, { method: 'POST', headers: requestHeaders }, (res) => {
      let text = ''; res.setEncoding('utf8'); res.on('data', (chunk) => { text += chunk; });
      res.on('end', () => resolve({ status: res.statusCode, text }));
    });
    req.on('error', reject); req.end(body);
  });
  assert.equal(response.status, status, name);
  assert.ok(response.text.includes(message), name);
  console.log(`PASS ${name}`);
}
