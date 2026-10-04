import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const state = join(homedir(), '.kyro', 'nebius-p1');
mkdirSync('docs/suivi/preuves', { recursive: true });
const pins = JSON.parse(readFileSync(join(state, 'pins.json'), 'utf8')).ipv4;
const container = 'kyro-nebius-p1-egress-1';
const report = { at: new Date().toISOString(), kind: 'independent_network_filter', rust_client_used: false, real_api_key_used: false, probes: [] };
function probe(name, command, expected) {
  const outcome = spawnSync('docker', ['run', '--rm', '--read-only', '--cap-drop=ALL', '--security-opt=no-new-privileges', '--user=10001:10001', '--network', `container:${container}`, '--entrypoint', command[0], 'kyro-nebius-guard:local', ...command.slice(1)], { encoding: 'utf8', timeout: 10_000 });
  const passed = expected(outcome);
  report.probes.push({ name, exit_code: outcome.status, http_status: /^\d{3}$/.test(outcome.stdout.trim()) ? outcome.stdout.trim() : null, passed });
}
const curl = ['curl', '--noproxy', '*', '--connect-timeout', '2', '--max-time', '3', '--silent', '--output', '/dev/null', '--write-out', '%{http_code}'];
probe('allowed_nebius_tls_no_auth', [...curl, '--resolve', `api.tokenfactory.nebius.com:443:${pins[0]}`, 'https://api.tokenfactory.nebius.com/v1/models'], (r) => r.status === 0 && r.stdout.trim() === '401');
for (const [name, url] of [
  ['public_destination_denied', 'https://1.1.1.1'],
  ['cloud_metadata_denied', 'http://169.254.169.254'],
  ['unapproved_private_denied', 'http://10.248.73.1'],
  ['runtime_dns_denied', 'https://example.com'],
  ['tcp_dns_denied', 'telnet://8.8.8.8:53'],
  ['ipv6_denied', 'https://[2606:4700:4700::1111]'],
]) probe(name, [...curl, url], (r) => r.status !== 0 && r.stdout.trim() === '000');
probe('allowed_postgres_tcp', ['sh', '-c', 'timeout 3 openssl s_client -starttls postgres -connect 10.248.73.2:5432 </dev/null >/dev/null 2>&1'], (r) => r.status === 0);
probe('wrong_tls_hostname_denied', [...curl, '--resolve', `wrong-host.invalid:443:${pins[0]}`, 'https://wrong-host.invalid/v1/models'], (r) => r.status !== 0 && r.stdout.trim() === '000');
const caps = spawnSync('docker', ['exec', container, 'sh', '-c', 'grep CapEff /proc/1/status'], { encoding: 'utf8' });
report.installer_dropped_capabilities = /CapEff:\s+0+/.test(caps.stdout);
report.passed = report.installer_dropped_capabilities && report.probes.every((item) => item.passed);
const stamp = report.at.replaceAll(/[^0-9A-Za-z]/g, '');
writeFileSync(`docs/suivi/preuves/nebius-network-${stamp}.json`, JSON.stringify(report, null, 2) + '\n');
process.stdout.write(JSON.stringify(report, null, 2) + '\n');
process.exitCode = report.passed ? 0 : 1;
