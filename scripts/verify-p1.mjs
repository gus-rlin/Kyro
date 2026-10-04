import { spawn, spawnSync } from 'node:child_process';
import { createServer, connect as tcpConnect } from 'node:net';
import { createHash, randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, rmdirSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { fileURLToPath } from 'node:url';
import { startSyntheticProvider } from './synthetic-provider.mjs';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const defaultDbContainer = 'kyro-p1-ops-postgres-1';
const defaultDbPort = 55440;
const postgresImage = 'postgres:18.6-alpine@sha256:77f585114c32fbca283dc835b0596f4e52b51b4c6662d7810b2f4084f60a1873';
const observedResponseBodies = [];
let auditCanary = null;
let testApiPort = 8080;
let testApiOrigin = 'http://127.0.0.1:8080';
let testProviderPort = 9090;
let testControlPort = 9091;
let syntheticRegistryPath = resolve(repoRoot, 'tests/fixtures/models.synthetic.e2e.json');
let syntheticRegistryDirectory = null;

function usage() {
  return `Usage:
  node scripts/verify-p1.mjs --help
  node scripts/verify-p1.mjs --fingerprint
  node scripts/verify-p1.mjs --preflight [--execution docker|local] [--db-container NAME] [--db-port PORT] [--admin-url LOOPBACK_URL]
  node scripts/verify-p1.mjs --run [--execution docker|local] [--db-container NAME] [--db-port PORT] [--admin-url LOOPBACK_URL] [--keep-on-failure] [--source-sha256 SHA256] [--api-port PORT] [--provider-port PORT] [--control-port PORT]
  node scripts/verify-p1.mjs --diagnose-p1-06 --database kyro_p1_e2e_<16-hex> --project-id UUID [--db-container NAME] [--db-port PORT]

--source-sha256 permits an uncommitted run only with the exact --fingerprint
build/test snapshot; input changes during the run invalidate its result.

--preflight performs read-only checks against either the specifically named
P1 operations PostgreSQL container (docker) or a loopback PostgreSQL service
(local). It creates no database.
--run creates only a disposable random kyro_p1_e2e_<hex> database, runs the
locked migrations and executes P1 acceptance against real API/worker binaries
and synthetic providers. It refuses to target a remote host or an unprefixed
database. Never pass a production URL or real provider credential.
--diagnose-p1-06 uses an existing disposable database without resetting it,
replays only the stopped-worker ApplyChanges admission, and records HTTP status,
stable error code, and the persisted job-count delta. It creates and revokes
one synthetic OIDC session; it never logs cookies, response bodies, or secrets.
`;
}

function parseArgs(args) {
  const parsed = {
    mode: null,
    execution: process.platform === 'win32' ? 'docker' : 'local',
    dbContainer: defaultDbContainer,
    dbPort: defaultDbPort,
    adminUrl: process.env.KYRO_DATABASE_ADMIN_URL ?? (process.platform === 'win32'
      ? 'postgresql://kyro_admin@127.0.0.1:55440/kyro_p1?sslmode=disable'
      : null),
    keepOnFailure: false,
    diagnosticDatabase: null,
    diagnosticProjectId: null,
    sourceSha256: null,
    apiPort: 8080,
    providerPort: 9090,
    controlPort: 9091,
  };
  for (let index = 0; index < args.length; index += 1) {
    const value = args[index];
    if (value === '--help' || value === '-h') parsed.mode = 'help';
    else if (value === '--preflight' || value === '--run' || value === '--diagnose-p1-06' || value === '--fingerprint') {
      if (parsed.mode) throw new Error('select exactly one execution mode');
      parsed.mode = value.slice(2);
    } else if (value === '--keep-on-failure') parsed.keepOnFailure = true;
    else if (value === '--db-container' || value === '--db-port' || value === '--execution' ||
        value === '--admin-url' || value === '--database' || value === '--project-id' || value === '--source-sha256' || value === '--api-port' || value === '--provider-port' || value === '--control-port') {
      const next = args[index + 1];
      if (!next || next.startsWith('--')) throw new Error(`${value} needs a value`);
      index += 1;
      if (value === '--db-container') parsed.dbContainer = next;
      else if (value === '--db-port') parsed.dbPort = Number(next);
      else if (value === '--execution') parsed.execution = next;
      else if (value === '--database') parsed.diagnosticDatabase = next;
      else if (value === '--project-id') parsed.diagnosticProjectId = next;
      else if (value === '--source-sha256') parsed.sourceSha256 = next;
      else if (value === '--api-port') parsed.apiPort = Number(next);
      else if (value === '--provider-port') parsed.providerPort = Number(next);
      else if (value === '--control-port') parsed.controlPort = Number(next);
      else parsed.adminUrl = next;
    } else throw new Error(`unknown option: ${value}`);
  }
  if (!parsed.mode) throw new Error('select --preflight or --run');
  if (!/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(parsed.dbContainer)) {
    throw new Error('database container name is invalid');
  }
  if (!Number.isInteger(parsed.dbPort) || parsed.dbPort < 1024 || parsed.dbPort > 65535) {
    throw new Error('--db-port must be between 1024 and 65535');
  }
  if (!['docker', 'local'].includes(parsed.execution)) {
    throw new Error('--execution must be docker or local');
  }
  if (parsed.mode === 'diagnose-p1-06' &&
      (parsed.execution !== 'docker' || !/^kyro_p1_e2e_[a-f0-9]{16}$/.test(parsed.diagnosticDatabase ?? '') ||
       !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(parsed.diagnosticProjectId ?? ''))) {
    throw new Error('--diagnose-p1-06 requires docker execution, an existing kyro_p1_e2e_<16 hex> database, and a project UUID');
  }
  if (parsed.mode !== 'diagnose-p1-06' && parsed.diagnosticProjectId !== null) {
    throw new Error('--project-id is only valid with --diagnose-p1-06');
  }
  if (parsed.sourceSha256 !== null && !/^[a-f0-9]{64}$/.test(parsed.sourceSha256)) {
    throw new Error('--source-sha256 must be a SHA-256 fingerprint');
  }
  if (![parsed.apiPort, parsed.providerPort, parsed.controlPort].every((port) => Number.isInteger(port) && port >= 1024 && port <= 65535) ||
      new Set([parsed.apiPort, parsed.providerPort, parsed.controlPort, parsed.dbPort]).size !== 4) {
    throw new Error('--api-port must be an unprivileged port distinct from database and providers');
  }
  return parsed;
}

function parseAdminUrl(raw) {
  if (!raw) throw new Error('local execution requires a loopback --admin-url or KYRO_DATABASE_ADMIN_URL');
  let url;
  try {
    url = new URL(raw);
  } catch {
    throw new Error('the database admin URL is invalid');
  }
  const loopbacks = new Set(['127.0.0.1', 'localhost', '::1']);
  if (!['postgres:', 'postgresql:'].includes(url.protocol) ||
      !loopbacks.has(url.hostname.replace(/^\[|\]$/g, '')) ||
      !url.username || url.password || !/^\/[A-Za-z0-9_]+$/.test(url.pathname) ||
      url.searchParams.has('password') || url.searchParams.has('user')) {
    throw new Error('database admin URL must use a loopback host, a named role, a database, and no embedded password');
  }
  const port = url.port ? Number(url.port) : 5432;
  if (!Number.isInteger(port) || port < 1024 || port > 65535) {
    throw new Error('database admin URL port is invalid');
  }
  return {
    host: url.hostname.replace(/^\[|\]$/g, ''),
    port,
    user: decodeURIComponent(url.username),
    database: url.pathname.slice(1),
    sslmode: url.searchParams.get('sslmode') ?? 'prefer',
  };
}

function command(file, args, { input, timeout = 30_000, allowFailure = false, env } = {}) {
  const result = spawnSync(file, args, {
    cwd: repoRoot,
    input,
    encoding: 'utf8',
    windowsHide: true,
    timeout,
    maxBuffer: 2 * 1024 * 1024,
    env,
  });
  if (result.error) throw new Error(`${file} could not run (${result.error.code ?? result.error.message})`);
  if (!allowFailure && result.status !== 0) {
    throw new Error(`${file} exited with status ${result.status ?? 'unknown'}`);
  }
  return result;
}

function docker(args, options) {
  return command('docker', args, options);
}

function captureSourceState() {
  const commit = command('git', ['rev-parse', 'HEAD']).stdout.trim();
  const dirtyPaths = command('git', ['status', '--porcelain']).stdout
    .split(/\r?\n/).filter(Boolean);
  // Hash tracked and new build/test inputs. Reports and journal updates are
  // deliberately excluded so evidence recording cannot change the tested code.
  const paths = command('git', ['ls-files', '--cached', '--others', '--exclude-standard', '-z']).stdout
    .split('\0').filter((path) => path && (
      /^(crates|scripts|tests|config|infra|\.github|docs\/backend)\//.test(path) ||
      /^(Cargo\.(toml|lock)|Dockerfile|\.dockerignore|rust-toolchain\.toml|compose[^/]*\.ya?ml)$/.test(path)
    ));
  const files = [...new Set(paths)].sort().map((path) => ({
    path, sha256: existsSync(resolve(repoRoot, path))
      ? createHash('sha256').update(readFileSync(resolve(repoRoot, path))).digest('hex')
      : null,
  }));
  return {
    commit,
    worktree_clean: dirtyPaths.length === 0,
    dirty_path_count: dirtyPaths.length,
    sha256: createHash('sha256').update(JSON.stringify(files)).digest('hex'),
    files,
  };
}

function parseJsonOutput(output, label) {
  try {
    return JSON.parse(output.trim());
  } catch {
    throw new Error(`${label} returned invalid JSON`);
  }
}

function assertManagedDatabase(inspect, expectedPort) {
  const labels = inspect.Config?.Labels ?? {};
  const nebiusFixture = inspect.Name === '/kyro-nebius-synthetic-db';
  const bindings = inspect.HostConfig?.PortBindings?.['5432/tcp'] ?? [];
  const dataVolumes = (inspect.Mounts ?? []).filter((mount) => mount.Destination === '/var/lib/postgresql');
  const safe = {
    container_running: inspect.State?.Status === 'running',
    health: inspect.State?.Health?.Status ?? inspect.State?.Status ?? 'unknown',
    compose_project: labels['com.docker.compose.project'] ?? null,
    compose_service: labels['com.docker.compose.service'] ?? null,
    image: inspect.Config?.Image ?? null,
    host_ip: bindings[0]?.HostIp ?? null,
    host_port: Number(bindings[0]?.HostPort ?? 0),
    data_volume: dataVolumes[0]?.Name ?? null,
  };
  if (safe.container_running !== true || safe.health !== 'healthy' ||
      safe.compose_project !== (nebiusFixture ? 'kyro-nebius-synthetic' : 'kyro-p1-ops') || safe.compose_service !== 'postgres' ||
      !/^postgres:18\.6-alpine@sha256:[a-f0-9]{64}$/.test(String(safe.image)) ||
      bindings.length !== 1 || safe.host_ip !== '127.0.0.1' || safe.host_port !== expectedPort ||
      dataVolumes.length !== 1 || safe.data_volume !== (nebiusFixture ? 'kyro-nebius-synthetic_postgres-data' : 'kyro-p1-ops_postgres-data')) {
    throw new Error('the selected PostgreSQL container did not match the isolated P1 operations resource identity');
  }
  return safe;
}

function checkLocalPortFree(port) {
  return new Promise((resolveFree, reject) => {
    const server = createServer();
    server.once('error', (error) => {
      if (error.code === 'EADDRINUSE') resolveFree(false);
      else reject(error);
    });
    server.listen(port, '127.0.0.1', () => server.close(() => resolveFree(true)));
  });
}

function checkDockerHostPort(port, host = 'host.docker.internal') {
  return new Promise((resolveReachable) => {
    const socket = tcpConnect({ host, port, timeout: 1500 });
    socket.once('connect', () => {
      socket.destroy();
      resolveReachable(true);
    });
    socket.once('timeout', () => {
      socket.destroy();
      resolveReachable(false);
    });
    socket.once('error', () => resolveReachable(false));
  });
}

async function runPreflight(options) {
  if (options.execution === 'local') return runLocalPreflight(options);

  const dockerVersion = docker(['version', '--format', '{{.Server.Version}}']).stdout.trim();
  const inspectOutput = docker(['inspect', options.dbContainer]).stdout;
  const inspect = parseJsonOutput(inspectOutput, 'docker inspect')[0];
  if (!inspect) throw new Error('the selected PostgreSQL container was not found');
  const databaseResource = assertManagedDatabase(inspect, options.dbPort);
  const version = docker([
    'exec', options.dbContainer, 'psql', '-X', '-v', 'ON_ERROR_STOP=1',
    '-U', 'kyro_admin', '-d', 'kyro_p1', '-At', '-c', 'SHOW server_version;',
  ]).stdout.trim();
  if (version !== '18.6') throw new Error(`expected PostgreSQL 18.6; observed ${version || 'no version'}`);
  const roles = docker([
    'exec', options.dbContainer, 'psql', '-X', '-v', 'ON_ERROR_STOP=1',
    '-U', 'kyro_admin', '-d', 'kyro_p1', '-At', '-F', '|', '-c',
    "SELECT rolname, rolsuper, rolbypassrls FROM pg_roles WHERE rolname LIKE 'kyro_%' ORDER BY 1;",
  ]).stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => {
    const [role, superuser, bypassRls] = line.split('|');
    return { role, superuser: superuser === 't', bypass_rls: bypassRls === 't' };
  });
  const dockerNetworkProbe = docker([
    'run', '--rm', '--network', 'bridge', 'postgres:18.6-alpine@sha256:77f585114c32fbca283dc835b0596f4e52b51b4c6662d7810b2f4084f60a1873',
    'pg_isready', '-h', 'host.docker.internal', '-p', String(options.dbPort),
    '-U', 'kyro_admin', '-d', 'kyro_p1',
  ], { allowFailure: true, timeout: 15_000 });
  if (dockerNetworkProbe.status !== 0) {
    throw new Error('a Linux container could not reach the loopback-published P1 PostgreSQL endpoint');
  }
  const appPorts = [testApiPort, testProviderPort, testControlPort];
  const portsAvailable = {};
  for (const port of appPorts) portsAvailable[String(port)] = await checkLocalPortFree(port);
  const report = {
    record: 'part1-e2e-preflight',
    status: 'passed_read_only',
    versions: { node: process.version, docker_server: dockerVersion, postgresql: version },
    database_resource: databaseResource,
    runtime_roles_observed_in_existing_db: roles,
    docker_to_postgres_probe: 'accepting connections',
    expected_loopback_ports_available: portsAvailable,
    database_created: false,
    restore_database_created: false,
    existing_database_modified: false,
    persistent_containers_created: 0,
    temporary_probe_containers_created: 1,
    temporary_probe_containers_removed: 1,
    real_secrets_used: false,
    acceptance_criteria_verified: [],
  };
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  if (Object.values(portsAvailable).some((available) => !available)) {
    process.stderr.write('One or more E2E loopback ports are occupied; --run must not start until the selected API port, 9090 and 9091 are free.\n');
    return 2;
  }
  return 0;
}

function pgEnvironment(admin) {
  const env = cleanEnvironment();
  return {
    ...env,
    PGHOST: admin.host,
    PGPORT: String(admin.port),
    PGUSER: admin.user,
    PGSSLMODE: admin.sslmode,
    PGCONNECT_TIMEOUT: '3',
  };
}

function cleanEnvironment() {
  const safe = {};
  for (const name of [
    'PATH', 'HOME', 'USERPROFILE', 'SYSTEMROOT', 'WINDIR', 'TEMP', 'TMP', 'TMPDIR',
    // PowerShell command resolution and Docker Compose discovery on Windows.
    'PATHEXT', 'PROGRAMFILES',
    'LANG', 'LC_ALL', 'TZ', 'SSL_CERT_FILE', 'SSL_CERT_DIR',
  ]) {
    if (process.env[name] !== undefined) safe[name] = process.env[name];
  }
  return safe;
}

function targetDatabaseUrl(raw, role, database, applicationName, { dockerHost = null } = {}) {
  const url = new URL(raw);
  url.username = role;
  url.pathname = `/${database}`;
  url.password = '';
  if (dockerHost) url.hostname = dockerHost;
  if (applicationName) url.searchParams.set('application_name', applicationName);
  return url.toString();
}

function safeIdentifier(value) {
  if (!/^[a-z][a-z0-9_]{0,62}$/.test(value)) throw new Error('generated database identifier is invalid');
  return `"${value}"`;
}

function lastText(chunks, cap = 128 * 1024) {
  const joined = Buffer.concat(chunks).toString('utf8');
  return joined.length <= cap ? joined : joined.slice(-cap);
}

function startChild(file, args, env = cleanEnvironment()) {
  const child = spawn(file, args, {
    cwd: repoRoot,
    env,
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  });
  const stdout = [];
  const stderr = [];
  let outBytes = 0;
  let errBytes = 0;
  child.stdout.on('data', (chunk) => {
    if (outBytes < 256 * 1024) {
      stdout.push(chunk);
      outBytes += chunk.length;
    }
  });
  child.stderr.on('data', (chunk) => {
    if (errBytes < 256 * 1024) {
      stderr.push(chunk);
      errBytes += chunk.length;
    }
  });
  const exited = new Promise((resolveExit) => {
    child.once('error', (error) => resolveExit({ code: null, errorCode: error.code ?? 'spawn-error' }));
    child.once('exit', (code, signal) => resolveExit({ code, signal }));
  });
  return { child, exited, logs: () => `${lastText(stdout)}\n${lastText(stderr)}` };
}

async function runChild(file, args, { env = cleanEnvironment(), timeout = 60_000, input } = {}) {
  const running = startChild(file, args, env);
  if (input !== undefined) {
    running.child.stdin?.end(input);
  }
  let timer;
  const outcome = await Promise.race([
    running.exited,
    new Promise((resolveExit) => {
      timer = setTimeout(() => {
        running.child.kill('SIGKILL');
        resolveExit({ code: null, signal: 'timeout' });
      }, timeout);
    }),
  ]);
  clearTimeout(timer);
  return { ...outcome, logs: running.logs() };
}

async function waitUntil(predicate, { timeoutMs = 15_000, intervalMs = 100, label = 'condition' } = {}) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await predicate();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, intervalMs));
  }
  throw new Error(`${label} was not reached${lastError ? ` (${lastError.message})` : ''}`);
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

let openApiCache;

function openApiDocument() {
  if (!openApiCache) {
    openApiCache = JSON.parse(readFileSync(resolve(repoRoot, 'docs/backend/partie-1/openapi.v1.json'), 'utf8'));
  }
  return openApiCache;
}

function resolveOpenApiRef(ref, document) {
  if (!ref.startsWith('#/')) throw new Error('OpenAPI schema uses a non-local reference');
  return ref.slice(2).split('/').map((part) => part.replaceAll('~1', '/').replaceAll('~0', '~'))
    .reduce((value, key) => value?.[key], document);
}

function validateSchema(value, schema, path, document, depth = 0) {
  if (depth > 64) throw new Error(`${path} exceeded the schema recursion limit`);
  if (schema.$ref) {
    const target = resolveOpenApiRef(schema.$ref, document);
    if (!target) throw new Error(`${path} references an absent OpenAPI component`);
    return validateSchema(value, target, path, document, depth + 1);
  }
  if (schema.allOf && !schema.allOf.every((item) => {
    try { validateSchema(value, item, path, document, depth + 1); return true; } catch { return false; }
  })) throw new Error(`${path} did not match every OpenAPI allOf member`);
  if (schema.oneOf) {
    const matches = schema.oneOf.filter((item) => {
      try { validateSchema(value, item, path, document, depth + 1); return true; } catch { return false; }
    }).length;
    if (matches !== 1) throw new Error(`${path} did not match exactly one OpenAPI oneOf member`);
  }
  if (schema.anyOf && !schema.anyOf.some((item) => {
    try { validateSchema(value, item, path, document, depth + 1); return true; } catch { return false; }
  })) throw new Error(`${path} did not match any OpenAPI anyOf member`);
  if (schema.const !== undefined && !isDeepStrictEqual(value, schema.const)) {
    throw new Error(`${path} did not match its OpenAPI const`);
  }
  if (schema.enum && !schema.enum.some((item) => isDeepStrictEqual(value, item))) {
    throw new Error(`${path} was outside its OpenAPI enum`);
  }

  const types = schema.type === undefined ? [] : Array.isArray(schema.type) ? schema.type : [schema.type];
  if (types.length && !types.some((type) => (
    (type === 'null' && value === null) ||
    (type === 'object' && value !== null && typeof value === 'object' && !Array.isArray(value)) ||
    (type === 'array' && Array.isArray(value)) ||
    (type === 'string' && typeof value === 'string') ||
    (type === 'boolean' && typeof value === 'boolean') ||
    (type === 'integer' && Number.isSafeInteger(value)) ||
    (type === 'number' && typeof value === 'number' && Number.isFinite(value))
  ))) throw new Error(`${path} had the wrong OpenAPI type`);

  if (value === null) return;
  if (typeof value === 'string') {
    if (schema.minLength !== undefined && value.length < schema.minLength) throw new Error(`${path} was shorter than OpenAPI minLength`);
    if (schema.maxLength !== undefined && value.length > schema.maxLength) throw new Error(`${path} exceeded OpenAPI maxLength`);
    if (schema.pattern && !(new RegExp(schema.pattern).test(value))) throw new Error(`${path} did not match OpenAPI pattern`);
    if (schema.format === 'uuid' && !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)) {
      throw new Error(`${path} was not a UUID`);
    }
    if (schema.format === 'date-time' && (!Number.isFinite(Date.parse(value)) || !/[Tt].*(Z|[+-]\d\d:\d\d)$/.test(value))) {
      throw new Error(`${path} was not an RFC 3339 date-time`);
    }
  }
  if (typeof value === 'number') {
    if (schema.minimum !== undefined && value < schema.minimum) throw new Error(`${path} was below OpenAPI minimum`);
    if (schema.maximum !== undefined && value > schema.maximum) throw new Error(`${path} exceeded OpenAPI maximum`);
  }
  if (Array.isArray(value)) {
    if (schema.minItems !== undefined && value.length < schema.minItems) throw new Error(`${path} had too few OpenAPI items`);
    if (schema.maxItems !== undefined && value.length > schema.maxItems) throw new Error(`${path} exceeded OpenAPI maxItems`);
    if (schema.items) value.forEach((item, index) => validateSchema(item, schema.items, `${path}[${index}]`, document, depth + 1));
  }
  if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
    for (const key of schema.required ?? []) {
      if (!Object.hasOwn(value, key)) throw new Error(`${path} omitted required OpenAPI property ${key}`);
    }
    for (const [key, item] of Object.entries(value)) {
      if (schema.properties?.[key]) {
        validateSchema(item, schema.properties[key], `${path}.${key}`, document, depth + 1);
      } else if (schema.additionalProperties === false) {
        throw new Error(`${path} contained undeclared OpenAPI property ${key}`);
      } else if (schema.additionalProperties && typeof schema.additionalProperties === 'object') {
        validateSchema(item, schema.additionalProperties, `${path}.${key}`, document, depth + 1);
      }
    }
  }
}

function assertOpenApiComponent(name, value) {
  const document = openApiDocument();
  const schema = document.components?.schemas?.[name];
  assert(schema, `OpenAPI component ${name} is missing`);
  try {
    validateSchema(value, schema, name, document);
  } catch (error) {
    throw new Error(`${name} response did not match its OpenAPI schema (${error.message})`);
  }
}

function assertOpenApiError(result, label) {
  if (result.response.status < 400) return;
  try {
    assertOpenApiComponent('ErrorEnvelope', result.data);
  } catch (error) {
    throw new Error(`${label} error response did not match OpenAPI (${error.message})`);
  }
}

async function requestJson(url, { method = 'GET', headers = {}, body, signal, maxBytes = 1024 * 1024 } = {}) {
  const response = await fetch(url, {
    method,
    headers: { ...(body === undefined ? {} : { 'content-type': 'application/json' }), ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
    redirect: 'manual',
    signal: signal ?? AbortSignal.timeout(10_000),
  });
  let text = '';
  if (response.body) {
    const reader = response.body.getReader();
    const chunks = [];
    let size = 0;
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.length;
        if (size > maxBytes) {
          await reader.cancel().catch(() => {});
          throw new Error('HTTP response exceeded the test client body limit');
        }
        chunks.push(Buffer.from(value));
      }
      text = Buffer.concat(chunks).toString('utf8');
    } finally {
      reader.releaseLock();
    }
  }
  let data = null;
  if (text) {
    try { data = JSON.parse(text); } catch { data = text; }
  }
  observedResponseBodies.push(text);
  return { response, data, text };
}

function setCookieValues(headers) {
  if (typeof headers.getSetCookie === 'function') return headers.getSetCookie();
  const combined = headers.get('set-cookie');
  return combined ? combined.split(/, (?=[^;,]+=)/) : [];
}

function cookieFrom(headers, name) {
  const matches = setCookieValues(headers).filter((cookie) => cookie.startsWith(`${name}=`));
  return matches.length ? matches[matches.length - 1].split(';', 1)[0].slice(name.length + 1) : null;
}

async function controlRequest(provider, token, path, body) {
  const { response, data } = await requestJson(`${provider.controlOrigin}${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: { authorization: `Bearer ${token}` },
    body,
    maxBytes: 32 * 1024,
  });
  assert(response.ok, 'synthetic provider control request failed');
  return data;
}

async function providerSnapshot(provider) {
  return provider.remoteControlToken
    ? controlRequest(provider, provider.remoteControlToken, '/__e2e/snapshot')
    : provider.snapshot();
}

function parsePrometheusMetrics(exposition) {
  const samples = new Map();
  for (const line of exposition.split(/\r?\n/)) {
    if (!line || line.startsWith('#')) continue;
    const match = line.match(/^([a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{([^}]*)\})?\s+([^\s]+)$/);
    if (!match) throw new Error('Prometheus exposition contained a malformed sample');
    const [, name, labelText = '', rawValue] = match;
    const labels = {};
    if (labelText) {
      for (const item of labelText.split(',')) {
        const label = item.match(/^([a-zA-Z_][a-zA-Z0-9_]*)="([^"\\]*(?:\\.[^"\\]*)*)"$/);
        if (!label) throw new Error('Prometheus exposition contained a malformed label');
        labels[label[1]] = label[2];
      }
    }
    const value = Number(rawValue);
    if (!Number.isFinite(value) || samples.has(`${name}{${labelText}}`)) {
      throw new Error('Prometheus exposition contained a duplicate or nonnumeric sample');
    }
    samples.set(`${name}{${labelText}}`, { name, labels, value });
  }
  return samples;
}

async function readPrometheusMetrics(apiOrigin) {
  const response = await fetch(`${apiOrigin}/metrics`, { signal: AbortSignal.timeout(3000) });
  assert(response.status === 200, 'runtime metrics endpoint did not return HTTP 200');
  assert((response.headers.get('content-type') ?? '').includes('text/plain'),
    'runtime metrics endpoint did not return Prometheus text');
  const exposition = await response.text();
  assert(Buffer.byteLength(exposition) <= 16 * 1024, 'runtime metrics output exceeded the bounded sample limit');
  return { samples: parsePrometheusMetrics(exposition), exposition };
}

function metricValue(samples, metric, labels = {}) {
  const target = Object.entries(labels).map(([key, value]) => `${key}="${value}"`).join(',');
  const sample = samples.get(`${metric}{${target}}`);
  if (!sample) throw new Error(`runtime metrics omitted ${metric} with its bounded labels`);
  return sample.value;
}

async function configureProviderScenario(provider, scenario) {
  if (provider.remoteControlToken) {
    return controlRequest(provider, provider.remoteControlToken, '/__e2e/scenario', { inference: scenario });
  }
  return provider.setInferenceScenario(scenario);
}

async function releaseProviderInference(provider) {
  if (provider.remoteControlToken) {
    return controlRequest(provider, provider.remoteControlToken, '/__e2e/release', {});
  }
  return provider.releaseInference();
}

async function startDockerProvider({ apiKey, controlToken, runToken }) {
  const image = `kyro-p1-e2e-provider:${runToken}`;
  const container = `kyro-p1-e2e-provider-${runToken}`;
  docker(['build', '-f', 'tests/fixtures/Dockerfile.synthetic-provider', '-t', image, '.'], { timeout: 180_000 });
  try {
    docker([
      'run', '--detach', '--name', container,
      '--publish', `127.0.0.1:${testApiPort}:${testApiPort}`,
      '--publish', `127.0.0.1:${testProviderPort}:${testProviderPort}`,
      '--publish', `127.0.0.1:${testControlPort}:${testControlPort}`,
      '--env', `KYRO_E2E_PROVIDER_PORT=${testProviderPort}`,
      '--env', `KYRO_E2E_CONTROL_PORT=${testControlPort}`,
      '--env', `KYRO_E2E_CONTROL_TOKEN=${controlToken}`,
      '--env', `KYRO_MODEL_API_KEY=${apiKey}`,
      image,
    ]);
  } catch (error) {
    docker(['image', 'rm', image], { allowFailure: true });
    throw error;
  }
  const provider = {
    origin: `http://127.0.0.1:${testProviderPort}`,
    issuer: `http://127.0.0.1:${testProviderPort}/issuer`,
    authorizationEndpoint: `http://127.0.0.1:${testProviderPort}/oidc/authorize`,
    tokenEndpoint: `http://127.0.0.1:${testProviderPort}/oidc/token`,
    jwksUri: `http://127.0.0.1:${testProviderPort}/oidc/jwks`,
    inferenceBaseUrl: `http://127.0.0.1:${testProviderPort}/v1`,
    controlOrigin: `http://127.0.0.1:${testControlPort}`,
    clientId: 'kyro-e2e-client',
    model: 'synthetic-structured',
    remoteControlToken: controlToken,
    container,
    image,
    async close() {
      docker(['rm', '--force', container], { allowFailure: true });
      docker(['image', 'rm', image], { allowFailure: true });
    },
  };
  try {
    await waitUntil(async () => {
      try {
        return (await controlRequest(provider, controlToken, '/__e2e/ready')).ready === true;
      } catch {
        return false;
      }
    }, { timeoutMs: 30_000, intervalMs: 200, label: 'Docker synthetic provider readiness' });
  } catch (error) {
    await provider.close();
    throw error;
  }
  return provider;
}

function buildDockerAppImage(runToken) {
  if (!existsSync(resolve(repoRoot, 'Dockerfile'))) {
    throw new Error('Docker execution requires the integrated repository Dockerfile');
  }
  const image = `kyro-p1-e2e-app:${runToken}`;
  try {
    docker(['build', '-f', 'Dockerfile', '-t', image, '.'], { timeout: 900_000 });
  } catch (error) {
    docker(['image', 'rm', image], { allowFailure: true });
    throw error;
  }
  return image;
}

function dockerAdminUrl(adminUrl, database) {
  return targetDatabaseUrl(adminUrl, 'kyro_admin', database, null, { dockerHost: 'host.docker.internal' });
}

function executionAdmin(options) {
  const fallback = 'postgresql://kyro_admin@127.0.0.1:55440/kyro_p1?sslmode=disable';
  const admin = parseAdminUrl(options.adminUrl ?? fallback);
  if (options.execution === 'docker') {
    if (admin.host !== '127.0.0.1' || admin.port !== options.dbPort ||
        admin.user !== 'kyro_admin' || admin.database !== 'kyro_p1') {
      throw new Error('Docker execution is restricted to kyro_admin on the named P1 loopback PostgreSQL instance');
    }
    return { ...admin, execution: 'docker', container: options.dbContainer };
  }
  return { ...admin, execution: 'local' };
}

async function login(apiOrigin, provider, controlToken, subject, claimOverrides) {
  if (controlToken) {
    await controlRequest(provider, controlToken, '/__e2e/identity', {
      sub: subject,
      claims: claimOverrides ?? {},
    });
  }
  const loginResponse = await requestJson(`${apiOrigin}/v1/auth/login`);
  assert(loginResponse.response.status === 303, 'OIDC login did not redirect');
  const authorization = loginResponse.response.headers.get('location');
  assert(authorization, 'OIDC login omitted its authorization redirect');
  const authorizationUrl = new URL(authorization);
  const state = authorizationUrl.searchParams.get('state');
  const binding = cookieFrom(loginResponse.response.headers, 'kyro_oidc_binding');
  assert(state && binding, 'OIDC flow omitted its state or browser binding');

  const providerResponse = await requestJson(authorization);
  assert(providerResponse.response.status === 200, 'synthetic authorization request was refused');
  const { code } = providerResponse.data;
  assert(typeof code === 'string', 'synthetic authorization code was missing');
  const callbackUrl = new URL('/v1/auth/callback', apiOrigin);
  callbackUrl.searchParams.set('code', code);
  callbackUrl.searchParams.set('state', state);
  const callback = await requestJson(callbackUrl, {
    headers: { cookie: `kyro_oidc_binding=${binding}` },
  });
  const sessionCookie = cookieFrom(callback.response.headers, 'kyro_session');
  const csrfCookie = cookieFrom(callback.response.headers, 'kyro_csrf');
  if (claimOverrides) {
    assert(!sessionCookie && [400, 401, 403].includes(callback.response.status), 'invalid OIDC claims created a session');
    const absent = await requestJson(`${apiOrigin}/v1/auth/session`);
    assert(absent.response.status === 401, 'callback rejection left an authenticated session');
    return { rejected: true, callbackStatus: callback.response.status };
  }
  assert(callback.response.status === 303 && sessionCookie && csrfCookie, 'valid OIDC callback did not create opaque cookies');
  assert(sessionCookie.split('.').length !== 3, 'session cookie looks like a JWT');
  const session = await requestJson(`${apiOrigin}/v1/auth/session`, {
    headers: { cookie: `kyro_session=${sessionCookie}; kyro_csrf=${csrfCookie}` },
  });
  assert(session.response.status === 200 && session.data?.actor_id && session.data?.csrf_token,
    'opaque session cookie did not resolve to a persisted actor');
  return {
    actorId: session.data.actor_id,
    csrf: session.data.csrf_token,
    sessionCookie,
    csrfCookie,
    cookieHeader: `kyro_session=${sessionCookie}; kyro_csrf=${csrfCookie}`,
    callbackCode: code,
    callbackState: state,
    browserBinding: binding,
  };
}

async function apiRequest(apiOrigin, path, session, { method = 'GET', body, headers = {}, noCsrf = false } = {}) {
  const requestHeaders = { ...headers };
  if (session) requestHeaders.cookie = session.cookieHeader;
  if (session && !noCsrf && !['GET', 'HEAD', 'OPTIONS'].includes(method)) {
    requestHeaders['x-csrf-token'] = session.csrf;
    requestHeaders.origin ??= apiOrigin;
  }
  const result = await requestJson(`${apiOrigin}${path}`, { method, body, headers: requestHeaders });
  if (result.response.status >= 400) assertOpenApiError(result, `${method} ${path}`);
  return result;
}

async function waitForJob(apiOrigin, projectId, jobId, session, statuses, timeoutMs = 15_000) {
  return waitUntil(async () => {
    const result = await apiRequest(apiOrigin, `/v1/projects/${projectId}/jobs/${jobId}`, session);
    assert(result.response.ok, 'job lookup failed while waiting for a terminal state');
    assertOpenApiComponent('JobView', result.data);
    return statuses.includes(result.data.status) ? result.data : null;
  }, { timeoutMs, intervalMs: 100, label: `job ${statuses.join('/')}` });
}

async function enqueueJob(apiOrigin, projectId, session, payload, revision, key, extras = {}) {
  const result = await apiRequest(apiOrigin, `/v1/projects/${projectId}/jobs`, session, {
    method: 'POST',
    headers: { 'if-match': `"rev-${revision}"`, 'idempotency-key': key },
    body: { payload, ...extras },
  });
  if (result.response.status === 202) assertOpenApiComponent('JobView', result.data);
  return result;
}

async function reconcileEffect(apiOrigin, projectId, effectId, session, evidenceId, decision, key) {
  const result = await apiRequest(apiOrigin, `/v1/projects/${projectId}/effects/${effectId}/reconcile`, session, {
    method: 'POST',
    headers: { 'idempotency-key': key },
    body: { evidence_id: evidenceId, decision },
  });
  if (result.response.status === 202) assertOpenApiComponent('JobView', result.data);
  return result;
}

async function setBudgetLimit(apiOrigin, projectId, session, limitUnits) {
  const before = await apiRequest(apiOrigin, `/v1/projects/${projectId}/budget`, session);
  assert(before.response.status === 200, 'project budget was not readable before CAS');
  assertOpenApiComponent('BudgetSnapshot', before.data);
  const etag = before.response.headers.get('etag');
  assert(etag && /^"?budget-\d+"?$/.test(etag), 'budget snapshot omitted a version ETag');
  const changed = await apiRequest(apiOrigin, `/v1/projects/${projectId}/budget`, session, {
    method: 'PUT',
    headers: { 'if-match': etag },
    body: { limit_units: limitUnits, currency: 'SYN', unit_scale: 1 },
  });
  assert(changed.response.status === 200, 'budget CAS did not accept its current strong version');
  assertOpenApiComponent('BudgetSnapshot', changed.data);
  return changed.data;
}

async function readSseEvent(response, timeoutMs = 3000) {
  assert(response.body, 'SSE response body is absent');
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let text = '';
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      let timer;
      const read = await Promise.race([
        reader.read(),
        new Promise((resolveRead) => {
          timer = setTimeout(() => resolveRead({ timeout: true }), Math.max(1, deadline - Date.now()));
        }),
      ]);
      clearTimeout(timer);
      if (read.timeout) break;
      if (read.done) break;
      text += decoder.decode(read.value, { stream: true });
      const separator = text.indexOf('\n\n');
      if (separator >= 0) return text.slice(0, separator);
    }
    throw new Error('no SSE event arrived before the bounded test deadline');
  } finally {
    await reader.cancel().catch(() => {});
  }
}

async function readSseEventOrTimeout(response, timeoutMs = 1000) {
  assert(response.body, 'SSE response body is absent');
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let text = '';
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      let timer;
      const read = await Promise.race([
        reader.read(),
        new Promise((resolveRead) => {
          timer = setTimeout(() => resolveRead({ timeout: true }), Math.max(1, deadline - Date.now()));
        }),
      ]);
      clearTimeout(timer);
      if (read.timeout || read.done) return null;
      text += decoder.decode(read.value, { stream: true });
      const separator = text.indexOf('\n\n');
      if (separator >= 0) return text.slice(0, separator);
    }
    return null;
  } finally {
    await reader.cancel().catch(() => {});
  }
}

function writeEvidence(report, runId) {
  const path = resolve(repoRoot, 'docs/suivi/preuves', `part1-e2e-${runId}.json`);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx' });
  return path;
}

async function runP106Diagnostic(options) {
  const runToken = randomBytes(8).toString('hex');
  const runId = `${new Date().toISOString().replace(/[-:.]/g, '').slice(0, 15)}-p106-${runToken}`;
  const database = options.diagnosticDatabase;
  const projectId = options.diagnosticProjectId;
  const report = {
    record: 'part1-e2e-p1-06-admission-diagnostic',
    run_id: runId,
    started_at: new Date().toISOString(),
    status: 'running',
    source: captureSourceState(),
    execution: 'docker',
    database_name: database,
    command: [
      'node scripts/verify-p1.mjs', '--diagnose-p1-06', '--execution', 'docker',
      '--db-container', options.dbContainer, '--db-port', String(options.dbPort),
      '--database', database, '--project-id', projectId,
    ],
    database_reset: false,
    migrations_run: false,
    real_secrets_used: false,
    runtime_image_id: null,
    services: { postgres: 'real PostgreSQL 18.6', api: 'real binary', worker: 'real binary', oidc: 'synthetic mock' },
    worker_config: {
      poll_ms: 25, poll_min_ms: 10, poll_max_ms: 10_000,
      lease_seconds: 2, lease_min_seconds: 2, lease_max_seconds: 120,
      within_config_bounds: true,
    },
    worker: { started: false, idle_connection_observed: false, stopped: false, exit_code: null },
    session: { synthetic_actor_resolved: false, created: false, revoked: false, logout_http_status: null },
    admission: {
      method: 'POST',
      route: '/v1/projects/{project_id}/jobs',
      payload_kind: 'apply_changes',
      operation: 'add_node',
      project_id: projectId,
      revision: null,
      http_status: null,
      error_code: null,
      response_job_status: null,
      error_envelope_matches_openapi: null,
    },
    project_read: { status: null, authorized_with_synthetic_session: false },
    jobs_before: null,
    jobs_after: null,
    job_row_delta: null,
    synthetic_provider_inference_requests: null,
    failure_stage: null,
    evidence_path: null,
  };
  let admin = null;
  let provider = null;
  let apiProcess = null;
  let workerProcess = null;
  let session = null;
  let dockerContext = null;
  let stage = 'preflight';
  try {
    if (!/^kyro_p1_e2e_[a-f0-9]{16}$/.test(database)) {
      throw new Error('diagnostic database name is outside the disposable prefix');
    }
    stage = 'database_exists';
    admin = executionAdmin(options);
    const adminUrl = options.adminUrl ?? 'postgresql://kyro_admin@127.0.0.1:55440/kyro_p1?sslmode=disable';
    const exists = runPsql(admin, admin.database,
      `SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname='${database}');`);
    assert(exists === 't', 'preserved diagnostic database was not found');
    stage = 'api_role_precondition';
    const roles = runPsql(admin, database,
      "SELECT rolsuper || '|' || rolbypassrls FROM pg_roles WHERE rolname='kyro_api';");
    assert(roles === 'false|false', 'API runtime role was not non-admin');
    stage = 'baseline_job_count';
    const jobsBefore = Number(runPsql(admin, database,
      `SELECT count(*) FROM public.jobs WHERE project_id='${projectId}'::uuid;`));
    assert(jobsBefore === 0, 'project already contained a job; diagnostic would not be an exact reproduction');
    report.jobs_before = jobsBefore;

    stage = 'build';
    dockerContext = { appImage: buildDockerAppImage(runToken) };
    report.runtime_image_id = docker(['image', 'inspect', '--format', '{{.Id}}', dockerContext.appImage]).stdout.trim();
    const key = randomBytes(32).toString('base64url');
    const controlToken = randomBytes(32).toString('base64url');

    stage = 'provider_start';
    provider = await startDockerProvider({ apiKey: key, controlToken, runToken });
    dockerContext.providerContainer = provider.container;

    stage = 'api_start';
    const dockerHost = 'host.docker.internal';
    const apiDatabaseUrl = targetDatabaseUrl(adminUrl, 'kyro_api', database, null, { dockerHost });
    const workerApplication = `kyro_e2e_${runToken}_worker`;
    const workerDatabaseUrl = targetDatabaseUrl(adminUrl, 'kyro_worker', database, workerApplication, { dockerHost });
    apiProcess = await startApiProcess(apiDatabaseUrl, provider, runToken, dockerContext);
    report.api_ready = true;

    stage = 'worker_idle_stop';
    workerProcess = startWorkerProcess(workerDatabaseUrl, key, runToken, {
      pollMs: 25, leaseSeconds: 2, dockerContext,
    });
    await waitUntil(() => serviceIsRunning(workerProcess),
      { timeoutMs: 3000, intervalMs: 50, label: 'P1-06 diagnostic worker process start' });
    await waitForWorkerIdleConnection(admin, database, workerApplication);
    report.worker.started = true;
    report.worker.idle_connection_observed = true;
    const workerStop = await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    report.worker.stopped = workerStop?.signal === 'SIGTERM' && workerStop?.code === 0;
    report.worker.exit_code = workerStop?.code ?? null;
    assert(report.worker.stopped, 'P1-06 diagnostic worker did not stop cleanly');

    stage = 'synthetic_login';
    session = await login(testApiOrigin, provider, controlToken, 'synthetic-user-a');
    report.session.synthetic_actor_resolved = true;
    report.session.created = true;

    stage = 'project_snapshot';
    const project = await apiRequest(testApiOrigin, `/v1/projects/${projectId}`, session);
    report.project_read.status = project.response.status;
    report.project_read.authorized_with_synthetic_session = project.response.status === 200;
    report.admission.revision = Number(project.data?.project?.current_revision);
    assert(project.response.status === 200 && Number.isInteger(report.admission.revision),
      'diagnostic project snapshot was not readable');

    stage = 'job_admission';
    const admission = await requestJson(`${testApiOrigin}/v1/projects/${projectId}/jobs`, {
      method: 'POST',
      headers: {
        cookie: session.cookieHeader,
        'x-csrf-token': session.csrf,
        origin: testApiOrigin,
        'if-match': `"rev-${report.admission.revision}"`,
        'idempotency-key': 'e2e-worker-durable-job',
      },
      body: {
        payload: { kind: 'apply_changes', changes: { operations: [{
          op: 'add_node',
          node: { id: 'e2e-worker-durable', kind: 'text.heading', properties: { text: 'durable' } },
        }] } },
      },
    });
    report.admission.http_status = admission.response.status;
    report.admission.error_code = admission.data?.error?.code ?? null;
    report.admission.response_job_status = admission.data?.status ?? null;
    if (admission.response.status >= 400) {
      try {
        assertOpenApiError(admission, 'P1-06 diagnostic POST jobs');
        report.admission.error_envelope_matches_openapi = true;
      } catch {
        report.admission.error_envelope_matches_openapi = false;
      }
    }
    if (admission.response.status === 202) assertOpenApiComponent('JobView', admission.data);
    report.jobs_after = Number(runPsql(admin, database,
      `SELECT count(*) FROM public.jobs WHERE project_id='${projectId}'::uuid;`));
    report.job_row_delta = report.jobs_after - report.jobs_before;
    report.synthetic_provider_inference_requests = (await providerSnapshot(provider)).inferenceRequests;
    report.result = admission.response.status === 202 && admission.data?.status === 'pending' && report.job_row_delta === 1
      ? 'admitted_and_persisted'
      : 'http_refused_before_job_persistence';
    report.status = 'diagnostic_completed';
  } catch (error) {
    report.status = 'diagnostic_failed';
    report.failure_stage = stage;
    report.failure_type = error instanceof Error ? error.name : 'unknown';
  } finally {
    if (session && apiProcess) {
      try {
        const logout = await apiRequest(testApiOrigin, '/v1/auth/logout', session, { method: 'POST' });
        report.session.logout_http_status = logout.response.status;
        report.session.revoked = logout.response.status === 204 || logout.response.status === 200;
      } catch {}
    }
    await stopChild(workerProcess, 'SIGTERM').catch(() => {});
    await stopChild(apiProcess, 'SIGTERM').catch(() => {});
    if (provider) {
      try { report.synthetic_provider_inference_requests ??= (await providerSnapshot(provider)).inferenceRequests; } catch {}
      await provider.close().catch(() => {});
    }
    if (dockerContext?.appImage) docker(['image', 'rm', dockerContext.appImage], { allowFailure: true });
    report.finished_at = new Date().toISOString();
    try {
      report.evidence_path = relative(repoRoot, writeEvidence(report, runId)).replaceAll('\\', '/');
    } catch {
      report.evidence_path = null;
      report.status = 'diagnostic_failed';
      report.failure_stage ??= 'evidence_write';
    }
  }
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  return report.status === 'diagnostic_completed' ? 0 : 1;
}

function migrationBinary() {
  return resolve(repoRoot, 'target', 'debug', process.platform === 'win32' ? 'kyro-migrate.exe' : 'kyro-migrate');
}

function runtimeBinary(name) {
  return resolve(repoRoot, 'target', 'debug', process.platform === 'win32' ? `${name}.exe` : name);
}

async function runMigrations(adminUrl, report, dockerContext = null) {
  const env = cleanEnvironment();
  env.KYRO_DATABASE_ADMIN_URL = adminUrl;
  const migrate = () => dockerContext
    ? runChild('docker', [
      'run', '--rm', '--network', 'bridge',
      '--env', `KYRO_DATABASE_ADMIN_URL=${adminUrl}`,
      '--entrypoint', 'kyro-migrate', dockerContext.appImage,
    ], { env: cleanEnvironment(), timeout: 120_000 })
    : runChild(migrationBinary(), [], { env, timeout: 120_000 });
  const outcomes = await Promise.all([migrate(), migrate()]);
  const exitCode = (outcome) => outcome.code ?? outcome.status;
  report.migrations.concurrent_runs = outcomes.map((outcome) => ({
    exit_code: exitCode(outcome),
    signal: outcome.signal ?? null,
  }));
  if (outcomes.some((outcome) => exitCode(outcome) !== 0)) {
    throw new Error('concurrent locked migration executions did not both succeed');
  }
  const replay = await migrate();
  report.migrations.replay_exit_code = exitCode(replay);
  if (exitCode(replay) !== 0) throw new Error('migration replay did not succeed');
}

function startDockerRuntime(container, entrypoint, env, dockerContext) {
  const fixturePath = syntheticRegistryPath;
  const executableByName = {
    'kyro-api': '/usr/local/bin/kyro-api',
    'kyro-worker': '/usr/local/bin/kyro-worker',
  };
  const executable = executableByName[entrypoint];
  if (!executable) throw new Error('unsupported Docker runtime entrypoint');
  const existingNames = docker(['ps', '-a', '--format', '{{.Names}}']).stdout
    .split(/\r?\n/).filter(Boolean);
  if (existingNames.includes(container)) {
    throw new Error('generated runtime container name already exists; refusing to reuse it');
  }
  const args = [
    'run', '--detach', '--name', container,
    '--network', `container:${dockerContext.providerContainer}`,
    '--volume', `${fixturePath}:/tmp/models.synthetic.e2e.json:ro`,
    '--entrypoint', executable,
  ];
  for (const [name, value] of Object.entries(env)) {
    if (name.startsWith('KYRO_') || name === 'RUST_LOG') args.push('--env', `${name}=${value}`);
  }
  args.push(dockerContext.appImage);
  try {
    docker(args);
    const [entrypointJson, environmentJson] = docker([
      'inspect', '--format', '{{json .Config.Entrypoint}}|{{json .Config.Env}}', container,
    ]).stdout.trim().split('|');
    const configuredEntrypoint = parseJsonOutput(entrypointJson, 'runtime entrypoint inspection');
    const configuredEnvironment = parseJsonOutput(environmentJson, 'runtime environment inspection');
    const windowsPath = Array.isArray(configuredEnvironment) && configuredEnvironment.some((item) =>
      typeof item === 'string' && item.startsWith('PATH=') && /^[A-Za-z]:[\\/]/.test(item.slice(5)));
    if (!Array.isArray(configuredEntrypoint) || configuredEntrypoint[0] !== executable || windowsPath) {
      throw new Error('runtime container inherited a host path or did not use its absolute executable');
    }
  } catch (error) {
    const inspected = docker(['inspect', container], { allowFailure: true });
    if (inspected.status === 0) {
      try {
        const [instance] = parseJsonOutput(inspected.stdout, 'runtime container ownership inspection');
        if (instance.Config?.Image === dockerContext.appImage &&
            instance.Config?.Entrypoint?.[0] === executable) {
          docker(['rm', '--force', container], { allowFailure: true });
        }
      } catch {
        // Keep an unidentifiable container for explicit inspection rather than deleting it.
      }
    }
    throw error;
  }
  return {
    dockerContainer: container,
    dockerEntrypoint: executable,
    hostWindowsPathForwarded: false,
    child: { exitCode: null, signalCode: null },
    logs: () => {
      const result = docker(['logs', container], { allowFailure: true });
      return `${result.stdout}\n${result.stderr}`;
    },
  };
}

function serviceIsRunning(service) {
  if (service?.dockerContainer) {
    const result = docker(['inspect', '--format', '{{.State.Running}}', service.dockerContainer], { allowFailure: true });
    const running = result.status === 0 && result.stdout.trim() === 'true';
    if (!running) service.child.exitCode = 1;
    return running;
  }
  return service?.child.exitCode === null && service?.child.signalCode === null;
}

async function startApiProcess(databaseUrl, provider, runToken, dockerContext = null) {
  const env = cleanEnvironment();
  Object.assign(env, {
    KYRO_ENV: 'development',
    KYRO_BIND: dockerContext ? `0.0.0.0:${testApiPort}` : `127.0.0.1:${testApiPort}`,
    KYRO_DATABASE_URL: databaseUrl,
    KYRO_MAX_CONNECTIONS: '1',
    KYRO_MAX_BODY_BYTES: '49152',
    KYRO_SYNTHETIC_PROVIDERS: 'true',
    KYRO_OIDC_ISSUER: provider.issuer,
    KYRO_OIDC_AUTHORIZATION_ENDPOINT: provider.authorizationEndpoint,
    KYRO_OIDC_TOKEN_ENDPOINT: provider.tokenEndpoint,
    KYRO_OIDC_JWKS_URI: provider.jwksUri,
    KYRO_OIDC_REDIRECT_URI: `${testApiOrigin}/v1/auth/callback`,
    KYRO_OIDC_CLIENT_ID: provider.clientId,
    KYRO_OIDC_SYNTHETIC_PROVIDER: 'true',
    KYRO_AUTH_UI_ORIGIN: testApiOrigin,
    KYRO_MODEL_REGISTRY_PATH: dockerContext ? '/tmp/models.synthetic.e2e.json' : syntheticRegistryPath,
    KYRO_MODEL_ALLOW_SYNTHETIC_LOOPBACK: '1',
    KYRO_RUN_ID: runToken,
    RUST_LOG: 'info',
  });
  const container = `kyro-p1-e2e-api-${runToken}`;
  const service = dockerContext
    ? startDockerRuntime(container, 'kyro-api', env, dockerContext)
    : startChild(runtimeBinary('kyro-api'), [], env);
  try {
    await waitUntil(async () => {
      if (!serviceIsRunning(service)) throw new Error('API exited before readiness');
      const response = await fetch(`${testApiOrigin}/health/ready`, { signal: AbortSignal.timeout(1000) });
      return response.status === 200;
    }, { timeoutMs: 30_000, intervalMs: 100, label: 'API readiness' });
  } catch (error) {
    await stopChild(service, 'SIGTERM').catch(() => {});
    throw error;
  }
  return service;
}

function startWorkerProcess(databaseUrl, apiKey, runToken, { pollMs = 25, leaseSeconds = 10, dockerContext = null } = {}) {
  const env = cleanEnvironment();
  Object.assign(env, {
    KYRO_ENV: 'development',
    KYRO_WORKER_DATABASE_URL: databaseUrl,
    KYRO_MAX_CONNECTIONS: '1',
    KYRO_WORKER_POLL_MS: String(pollMs),
    KYRO_LEASE_SECONDS: String(leaseSeconds),
    KYRO_SYNTHETIC_PROVIDERS: 'true',
    KYRO_MODEL_REGISTRY_PATH: dockerContext ? '/tmp/models.synthetic.e2e.json' : syntheticRegistryPath,
    KYRO_MODEL_API_KEY: apiKey,
    KYRO_MODEL_ALLOW_SYNTHETIC_LOOPBACK: '1',
    KYRO_RUN_ID: runToken,
    RUST_LOG: 'info',
  });
  if (dockerContext) return startDockerRuntime(`kyro-p1-e2e-worker-${runToken}-${randomBytes(3).toString('hex')}`, 'kyro-worker', env, dockerContext);
  return startChild(runtimeBinary('kyro-worker'), [], env);
}

async function stopChild(service, signal = 'SIGTERM', timeoutMs = 5000) {
  if (service?.dockerContainer) {
    const args = signal === 'SIGKILL'
      ? ['kill', '--signal', 'KILL', service.dockerContainer]
      : ['stop', '--time', String(Math.max(1, Math.floor(timeoutMs / 1000))), service.dockerContainer];
    const stopped = docker(args, { allowFailure: true, timeout: timeoutMs + 5000 });
    service.child.signalCode = signal;
    const exit = docker(['inspect', '--format', '{{.State.ExitCode}}', service.dockerContainer], { allowFailure: true });
    service.child.exitCode = exit.status === 0 ? Number(exit.stdout.trim()) : 1;
    docker(['rm', service.dockerContainer], { allowFailure: true });
    return { code: service.child.exitCode, signal: signal === 'SIGKILL' ? 'SIGKILL' : 'SIGTERM', stop_status: stopped.status };
  }
  if (!service || service.child.exitCode !== null || service.child.signalCode !== null) {
    return await service?.exited;
  }
  service.child.kill(signal);
  let timeoutHandle;
  const timeout = new Promise((resolveExit) => {
    timeoutHandle = setTimeout(() => resolveExit({ code: null, signal: 'stop-timeout' }), timeoutMs);
  });
  const result = await Promise.race([service.exited, timeout]);
  clearTimeout(timeoutHandle);
  if (result.signal === 'stop-timeout') {
    service.child.kill('SIGKILL');
    return await service.exited;
  }
  return result;
}

function parseProject(snapshot) {
  assertOpenApiComponent('ProjectSnapshot', snapshot);
  const project = snapshot?.project;
  assert(project && typeof project.id === 'string' && snapshot.revision,
    'project route did not return a project snapshot');
  return project;
}

async function createOrganization(apiOrigin, session, name) {
  const created = await apiRequest(apiOrigin, '/v1/organizations', session, {
    method: 'POST', body: { name },
  });
  assert(created.response.status === 201 && typeof created.data?.id === 'string',
    'organization creation did not return its synthetic identifier');
  return created.data;
}

const syntheticPolicy = {
  allowed_destinations: ['synthetic-local'],
  allowed_categories: ['user_request'],
  allowed_purposes: ['structured_extraction'],
  limits: {
    max_input_bytes: 65536,
    max_input_tokens: 16384,
    max_output_tokens: 2048,
    max_deadline_ms: 10000,
    max_response_bytes: 48000,
    max_retention_seconds: 0,
  },
};

const syntheticProjectLimits = {
  max_active_jobs: 8,
  max_queued_jobs: 32,
  max_job_attempts: 3,
  job_ttl_secs: 300,
  max_revisions: 1000,
};

async function createProject(apiOrigin, session, organizationId, name, {
  limits = syntheticProjectLimits,
  budgetLimitUnits = 1_000_000,
} = {}) {
  const created = await apiRequest(apiOrigin, '/v1/projects', session, {
    method: 'POST',
    body: {
      organization_id: organizationId,
      name,
      data_policy: syntheticPolicy,
      limits,
    },
  });
  assert(created.response.status === 201, 'project creation did not return HTTP 201');
  const project = parseProject(created.data);
  const budget = await apiRequest(apiOrigin, `/v1/projects/${project.id}/budget`, session);
  assert(budget.response.status === 200, 'new project budget was not readable');
  assertOpenApiComponent('BudgetSnapshot', budget.data);
  const etag = budget.response.headers.get('etag');
  assert(etag && /^"?budget-\d+"?$/.test(etag), 'budget response omitted its strong version ETag');
  const configured = await apiRequest(apiOrigin, `/v1/projects/${project.id}/budget`, session, {
    method: 'PUT',
    headers: { 'if-match': etag },
    body: { limit_units: budgetLimitUnits, currency: 'SYN', unit_scale: 1 },
  });
  assert(configured.response.status === 200, 'budget CAS could not set a synthetic test limit');
  assertOpenApiComponent('BudgetSnapshot', configured.data);
  return project;
}

async function applyChange(apiOrigin, projectId, session, revision, idempotencyKey, operations, extraHeaders = {}) {
  const result = await apiRequest(apiOrigin, `/v1/projects/${projectId}/changes`, session, {
    method: 'POST',
    headers: { 'if-match': `"rev-${revision}"`, 'idempotency-key': idempotencyKey, ...extraHeaders },
    body: { operations },
  });
  if (result.response.ok) assertOpenApiComponent('ApplyChangesResult', result.data);
  return result;
}

async function createTestDatabase(admin, runToken) {
  const database = `kyro_p1_e2e_${runToken}`;
  safeIdentifier(database);
  const exists = runPsql(admin, admin.database,
    `SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = '${database}');`);
  if (exists !== 'f') throw new Error('generated disposable database name already exists; refusing to reuse it');
  runPsql(admin, admin.database, `CREATE DATABASE ${safeIdentifier(database)};`);
  return database;
}

async function verifyRuntimeRoleAndRls(admin, database, adminUrl, report) {
  const roleRows = runPsql(admin, database,
    "SELECT rolname, rolsuper, rolbypassrls, rolcreaterole, rolcreatedb " +
    "FROM pg_roles WHERE rolname IN ('kyro_api', 'kyro_worker') ORDER BY rolname;")
    .split(/\r?\n/).filter(Boolean).map((line) => {
      const [role, superuser, bypassRls, createRole, createDb] = line.split('|');
      return { role, superuser: superuser === 't', bypass_rls: bypassRls === 't', create_role: createRole === 't', create_database: createDb === 't' };
    });
  if (roleRows.length !== 2 || roleRows.some((role) => role.superuser || role.bypass_rls || role.create_role || role.create_database)) {
    throw new Error('runtime database roles were missing or retained administrative privileges');
  }
  const rlsRows = runPsql(admin, database,
    "SELECT relname, relrowsecurity, relforcerowsecurity FROM pg_class " +
    "WHERE relnamespace='public'::regnamespace AND relkind='r' " +
    "AND relname IN ('projects','app_revisions','jobs','project_budgets','effects','budget_reservations','usage_ledger','events','outbox_events') " +
    "ORDER BY relname;")
    .split(/\r?\n/).filter(Boolean).map((line) => {
      const [table, enabled, forced] = line.split('|');
      return { table, rls_enabled: enabled === 't', rls_forced: forced === 't' };
    });
  if (rlsRows.length !== 9 || rlsRows.some((table) => !table.rls_enabled || !table.rls_forced)) {
    throw new Error('tenant data tables were missing forced row-level security');
  }
  const context = runPsql(admin, database,
    "BEGIN; SET LOCAL kyro.actor_id = '00000000-0000-4000-8000-000000000001'; " +
    "SET LOCAL kyro.environment = 'development'; COMMIT; " +
    "SELECT COALESCE(current_setting('kyro.actor_id',true),'') || '|' || count(*) FROM public.projects;",
    { user: 'kyro_api' });
  const [actorAfterCommit, visibleRows] = context.split('|');
  if (actorAfterCommit !== '' || Number(visibleRows) !== 0) {
    throw new Error('role connection retained actor context or exposed unscoped project rows');
  }
  report.database_security = {
    runtime_roles_non_admin: true,
    forced_rls_tables: rlsRows.length,
    runtime_context_cleared_after_commit: true,
    unscoped_project_rows_visible: 0,
  };
}

function blockTriggerSql({ functionName, triggerName, table, predicate }) {
  if (!/^[a-z][a-z0-9_]{0,62}$/.test(functionName) || !/^[a-z][a-z0-9_]{0,62}$/.test(triggerName) ||
      !['app_revisions', 'jobs', 'effects'].includes(table)) {
    throw new Error('test trigger identifier is invalid');
  }
  return `CREATE FUNCTION public."${functionName}"() RETURNS trigger LANGUAGE plpgsql AS $$ ` +
    `BEGIN IF ${predicate} THEN PERFORM pg_sleep(30); END IF; RETURN NEW; END $$; ` +
    `CREATE TRIGGER "${triggerName}" BEFORE INSERT OR UPDATE ON public.${table} ` +
    `FOR EACH ROW EXECUTE FUNCTION public."${functionName}"();`;
}

function installBlockTrigger(admin, database, definition) {
  runPsql(admin, database, blockTriggerSql(definition));
}

function removeBlockTrigger(admin, database, definition) {
  runPsql(admin, database,
    `DROP TRIGGER IF EXISTS "${definition.triggerName}" ON public.${definition.table}; ` +
    `DROP FUNCTION IF EXISTS public."${definition.functionName}"();`);
}

async function waitForWorkerSleep(admin, database, applicationName) {
  await waitUntil(() => {
    const sql = "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE usename='kyro_worker' " +
      `AND datname='${database}' AND application_name='${applicationName}' AND wait_event='PgSleep');`;
    return runPsql(admin, database, sql) === 't';
  }, { timeoutMs: 20_000, intervalMs: 100, label: 'worker transaction entering test pg_sleep trigger' });
}

async function waitForWorkerIdleConnection(admin, database, applicationName) {
  let consecutiveIdleSamples = 0;
  await waitUntil(() => {
    const sql = "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE usename='kyro_worker' " +
      `AND datname='${database}' AND application_name='${applicationName}' AND state='idle');`;
    consecutiveIdleSamples = runPsql(admin, database, sql) === 't'
      ? consecutiveIdleSamples + 1
      : 0;
    return consecutiveIdleSamples >= 2;
  }, { timeoutMs: 20_000, intervalMs: 100, label: 'worker idle readiness' });
}

function persistedJob(admin, database, jobId) {
  const rows = runPsql(admin, database,
    `SELECT status, generation, attempts FROM public.jobs WHERE id='${jobId}'::uuid;`);
  const [status, generation, attempts] = rows.split('|');
  if (!status) throw new Error('synthetic job was not persisted');
  return { status, generation: Number(generation), attempts: Number(attempts) };
}

function persistedEffect(admin, database, jobId) {
  const rows = runPsql(admin, database,
    `SELECT e.id, e.status, r.units, r.status FROM public.effects e ` +
    `JOIN public.budget_reservations r ON r.effect_id=e.id WHERE e.job_id='${jobId}'::uuid;`);
  if (!rows) return null;
  const [effectId, status, reservedUnits, reservationStatus] = rows.split('|');
  return { effectId, status, reservedUnits: Number(reservedUnits), reservationStatus };
}

async function waitForPersistedEffect(admin, database, jobId, statuses, timeoutMs = 15_000) {
  return waitUntil(() => {
    const effect = persistedEffect(admin, database, jobId);
    return effect && statuses.includes(effect.status) ? effect : null;
  }, { timeoutMs, intervalMs: 100, label: `persisted effect ${statuses.join('/')}` });
}

function lifecycleEventsForJob(admin, database, projectId, jobId) {
  for (const value of [projectId, jobId]) {
    if (!/^[0-9a-f-]{36}$/i.test(value)) throw new Error('synthetic event query received a non-UUID identifier');
  }
  const rows = runPsql(admin, database,
    `SELECT COALESCE(string_agg(type, ',' ORDER BY sequence), '') FROM public.events ` +
    `WHERE project_id='${projectId}'::uuid AND payload->>'job_id'='${jobId}';`);
  return rows ? rows.split(',') : [];
}

function assertJobLifecycleEvents(events, expectedTerminal, label) {
  const allowed = new Set([
    'job.queued', 'job.claimed', 'job.succeeded', 'job.failed', 'job.cancel_requested',
    'job.cancelled', 'job.unknown', 'job.stale', 'job.retry_scheduled', 'job.reconciled',
    'effect.prepared', 'effect.sending', 'effect.succeeded', 'effect.failed',
    'effect.cancelled', 'effect.unknown', 'effect.reconciled',
  ]);
  assert(events.length > 0 && events.every((event) => allowed.has(event)),
    `${label} emitted an unexpected or missing lifecycle event`);
  assert(events.includes(expectedTerminal), `${label} omitted its committed ${expectedTerminal} event`);
  return events;
}

function budgetCounters(admin, database, projectId) {
  if (!/^[0-9a-f-]{36}$/i.test(projectId)) throw new Error('synthetic budget query received a non-UUID identifier');
  const rows = runPsql(admin, database,
    `SELECT limit_units, reserved_units, spent_units FROM public.project_budgets ` +
    `WHERE project_id='${projectId}'::uuid;`);
  const [limitUnits, reservedUnits, spentUnits] = rows.split('|').map(Number);
  if (![limitUnits, reservedUnits, spentUnits].every(Number.isSafeInteger)) {
    throw new Error('synthetic project budget counters were missing or invalid');
  }
  return { limitUnits, reservedUnits, spentUnits };
}

function heldReservationUnits(admin, database, projectId) {
  if (!/^[0-9a-f-]{36}$/i.test(projectId)) throw new Error('synthetic reservation query received a non-UUID identifier');
  return Number(runPsql(admin, database,
    `SELECT COALESCE(sum(units),0) FROM public.budget_reservations ` +
    `WHERE project_id='${projectId}'::uuid AND status='held';`));
}

function countForJob(admin, database, table, jobId) {
  if (!['usage_ledger', 'budget_reservations', 'effects'].includes(table)) throw new Error('table is outside the test query allowlist');
  return Number(runPsql(admin, database,
    `SELECT count(*) FROM public.${table} WHERE job_id='${jobId}'::uuid;`));
}

async function waitForPersistedJob(admin, database, jobId, statuses, timeoutMs = 15_000) {
  return waitUntil(() => {
    const row = runPsql(admin, database,
      `SELECT status, generation, attempts FROM public.jobs WHERE id='${jobId}'::uuid;`);
    const [status, generation, attempts] = row.split('|');
    if (statuses.includes(status)) return { status, generation: Number(generation), attempts: Number(attempts) };
    return null;
  }, { timeoutMs, intervalMs: 100, label: `persisted job ${statuses.join('/')}` });
}

function syntheticModelRequest(content = 'Synthetic project title: Cedar.') {
  return {
    destination_id: 'synthetic-local',
    model: 'synthetic-structured',
    input: {
      purpose: 'structured_extraction',
      categories: ['user_request'],
      content: {
        instruction: 'Extract the title from this synthetic project description.',
        text: content,
      },
    },
    max_output_tokens: 64,
    deadline_ms: 10_000,
  };
}

async function waitForProviderCount(provider, count, timeoutMs = 12_000) {
  return waitUntil(() => {
    return providerSnapshot(provider).then((snapshot) => snapshot.inferenceRequests >= count ? snapshot : null);
  }, { timeoutMs, intervalMs: 25, label: 'synthetic inference request count' });
}

async function startSse(apiOrigin, projectId, session, cursor, extraHeaders = {}) {
  return fetch(`${apiOrigin}/v1/projects/${projectId}/events?after=${encodeURIComponent(cursor)}`, {
    headers: { cookie: session.cookieHeader, ...extraHeaders },
    redirect: 'manual',
    signal: AbortSignal.timeout(30_000),
  });
}

async function waitSseClosed(response, timeoutMs = 12_000) {
  assert(response.body, 'SSE response body is absent');
  const reader = response.body.getReader();
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      let timer;
      const result = await Promise.race([
        reader.read(),
        new Promise((resolveRead) => {
          timer = setTimeout(() => resolveRead({ timeout: true }), Math.max(1, deadline - Date.now()));
        }),
      ]);
      clearTimeout(timer);
      if (result.timeout) return false;
      if (result.done) return true;
    }
    return false;
  } finally {
    await reader.cancel().catch(() => {});
  }
}

function runPsql(admin, database, sql, { allowFailure = false, timeout = 15_000, user = admin.user } = {}) {
  const args = admin.execution === 'docker'
    ? ['exec', admin.container, 'psql', '-X', '-q', '-v', 'ON_ERROR_STOP=1', '-U', user, '-d', database, '-At', '-F', '|', '-c', sql]
    : ['-X', '-q', '-v', 'ON_ERROR_STOP=1', '-h', admin.host, '-p', String(admin.port),
      '-U', user, '-d', database, '-At', '-F', '|', '-c', sql];
  const result = admin.execution === 'docker'
    ? docker(args, { allowFailure, timeout })
    : command('psql', args, { allowFailure, timeout, env: pgEnvironment(admin) });
  return result.stdout.trim();
}

function runBinary(file, args, { input, env = cleanEnvironment(), timeout = 120_000 } = {}) {
  const result = spawnSync(file, args, {
    cwd: repoRoot,
    input,
    encoding: null,
    windowsHide: true,
    timeout,
    maxBuffer: 64 * 1024 * 1024,
    env,
  });
  if (result.error) throw new Error(`${file} could not run (${result.error.code ?? result.error.message})`);
  if (result.status !== 0) throw new Error(`${file} exited with status ${result.status ?? 'unknown'}`);
  return Buffer.from(result.stdout ?? []);
}

function restoredDataFingerprint(admin, database) {
  const sql = [
    "SELECT 'actors|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, issuer, subject, created_at FROM public.actors) q",
    "SELECT 'organizations|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, name, created_by, created_at FROM public.organizations) q",
    "SELECT 'memberships|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.organization_id, q.actor_id)), md5('')) FROM (SELECT organization_id, actor_id, role, created_by, created_at FROM public.memberships) q",
    "SELECT 'projects|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, organization_id, name, current_revision, event_sequence, data_policy, limits, created_by, created_at, updated_at FROM public.projects) q",
    "SELECT 'capability_grants|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, actor_id, project_id, actions, resources, environment, limits, expires_at, revoked_at, created_by, created_at FROM public.capability_grants) q",
    "SELECT 'app_revisions|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.project_id, q.revision)), md5('')) FROM (SELECT project_id, revision, spec, created_by, created_at FROM public.app_revisions) q",
    "SELECT 'change_commands|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.project_id, q.idempotency_key)), md5('')) FROM (SELECT project_id, idempotency_key, fingerprint, result, command_id, created_at FROM public.change_commands) q",
    "SELECT 'decisions|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, project_id, revision, actor_id, kind, payload, created_at FROM public.decisions) q",
    "SELECT 'jobs|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, project_id, actor_id, environment, source_revision, payload, attempts, max_attempts, deadline, cancel_requested, created_at FROM public.jobs) q",
    "SELECT 'effects|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, job_id, project_id, generation, destination, fingerprint, intent, result, reservation_id, created_at FROM public.effects) q",
    "SELECT 'budget_reservations|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, project_id, job_id, effect_id, idempotency_key, units, status, expires_at, created_at, updated_at FROM public.budget_reservations) q",
    "SELECT 'project_budgets|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.project_id)), md5('')) FROM (SELECT project_id, limit_units, reserved_units, spent_units, currency, unit_scale, updated_at FROM public.project_budgets) q",
    "SELECT 'usage_ledger|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, project_id, job_id, reservation_id, units, kind, provider, model, metadata, recorded_at FROM public.usage_ledger) q",
    "SELECT 'events|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.project_id, q.sequence)), md5('')) FROM (SELECT project_id, sequence, type, payload, actor_id, created_at FROM public.events) q",
    "SELECT 'outbox_events|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, project_id, event_sequence, topic, payload, available_at, delivered_at, attempts, created_at FROM public.outbox_events) q",
    "SELECT 'sessions|' || count(*) || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, token_hash, actor_id, csrf_hash, expires_at, created_at FROM public.sessions) q",
  ].join(' UNION ALL ');
  const rows = runPsql(admin, database, sql).split(/\r?\n/).filter(Boolean);
  const values = Object.fromEntries(rows.map((row) => {
    const [table, count, fingerprint] = row.split('|');
    if (!/^[a-z_]+$/.test(table) || !/^\d+$/.test(count) || !/^[a-f0-9]{32}$/.test(fingerprint)) {
      throw new Error('restore data fingerprint was malformed');
    }
    return [table, { rows: Number(count), md5: fingerprint }];
  }));
  if (Object.keys(values).length !== 16) throw new Error('restore data fingerprint omitted required P1 tables');
  return values;
}

async function backupAndRestoreFixture(admin, sourceDatabase, restoreDatabase, preparedJobId, sendingJobId, options, dockerContext) {
  safeIdentifier(sourceDatabase);
  safeIdentifier(restoreDatabase);
  if (!restoreDatabase.startsWith('kyro_restore_')) throw new Error('restore target did not use the isolated prefix');
  const tempDirectory = mkdtempSync(join(tmpdir(), 'kyro-p1-restore-'));
  const archivePath = join(tempDirectory, 'part1-synthetic.dump');
  try {
    let usedOperationsCli = false;
    const backupScript = resolve(repoRoot, 'scripts/backup-p1.ps1');
    const restoreScript = resolve(repoRoot, 'scripts/restore-p1.ps1');
    // The operations CLI targets its fixed managed container. Alternate isolated
    // fixtures use pg_dump/pg_restore below, never a different project's service.
    if (process.platform === 'win32' && options.execution === 'docker' && options.dbContainer === defaultDbContainer &&
        existsSync(backupScript) && existsSync(restoreScript)) {
      const backup = await runChild('pwsh', [
        '-NoLogo', '-NoProfile', '-File', backupScript,
        '-OutputFile', archivePath, '-Database', sourceDatabase,
      ], { env: cleanEnvironment(), timeout: 120_000 });
      if (backup.code !== 0 || !existsSync(archivePath) || statSync(archivePath).size === 0) {
        throw new Error('managed PostgreSQL backup CLI did not produce a nonempty archive');
      }
      const archive = readFileSync(archivePath);
      const digest = createHash('sha256').update(archive).digest('hex');
      const restore = await runChild('pwsh', [
        '-NoLogo', '-NoProfile', '-File', restoreScript,
        '-BackupFile', archivePath, '-ExpectedSha256', digest,
        '-TargetDatabase', restoreDatabase, '-SourceDatabase', sourceDatabase,
      ], { env: cleanEnvironment(), timeout: 180_000 });
      if (restore.code !== 0) throw new Error('managed PostgreSQL restore CLI did not complete the isolated restore');
      usedOperationsCli = true;
      const replay = await runChild('pwsh', [
        '-NoLogo', '-NoProfile', '-File', restoreScript,
        '-BackupFile', archivePath, '-ExpectedSha256', digest,
        '-TargetDatabase', restoreDatabase, '-SourceDatabase', sourceDatabase,
      ], { env: cleanEnvironment(), timeout: 60_000 });
      if (replay.code === 0 || !/already exists/i.test(replay.logs)) {
        throw new Error('managed restore CLI did not refuse a pre-existing target database');
      }
    } else {
      const env = pgEnvironment(admin);
      const dumpArgs = admin.execution === 'docker'
        ? ['exec', admin.container, 'pg_dump', '--format=custom', '--no-owner', '-U', admin.user, '-d', sourceDatabase]
        : ['--format=custom', '--no-owner', '-h', admin.host, '-p', String(admin.port), '-U', admin.user, '-d', sourceDatabase];
      const archive = runBinary(admin.execution === 'docker' ? 'docker' : 'pg_dump', dumpArgs, {
        env, timeout: 120_000,
      });
      if (archive.length === 0) throw new Error('pg_dump produced an empty database archive');
      writeFileSync(archivePath, archive, { flag: 'wx' });
      const digest = createHash('sha256').update(archive).digest('hex');
      const exists = runPsql(admin, admin.database,
        `SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname='${restoreDatabase}');`);
      if (exists !== 'f') throw new Error('restore target database already exists; refusing to reuse it');
      runPsql(admin, admin.database,
        `CREATE DATABASE ${safeIdentifier(restoreDatabase)} WITH OWNER ${safeIdentifier(admin.user)} TEMPLATE template0 ENCODING 'UTF8';`);
      const restoreArgs = admin.execution === 'docker'
        ? ['exec', '-i', admin.container, 'pg_restore', '--format=custom', '--no-owner', '--exit-on-error', '--single-transaction', '-U', admin.user, '-d', restoreDatabase]
        : ['--format=custom', '--no-owner', '--exit-on-error', '--single-transaction', '-h', admin.host,
          '-p', String(admin.port), '-U', admin.user, '-d', restoreDatabase];
      runBinary(admin.execution === 'docker' ? 'docker' : 'pg_restore', restoreArgs, {
        input: archive, env, timeout: 180_000,
      });
      const recoverRuntimeState = `
BEGIN;
DO $preflight$
BEGIN
    IF EXISTS (
        SELECT 1 FROM public.jobs j
        LEFT JOIN public.effects e ON e.job_id = j.id
        WHERE j.status IN ('pending', 'running')
          AND e.job_id IS NOT NULL
          AND (e.status = 'succeeded'
               OR (e.status = 'prepared' AND j.deadline > statement_timestamp() AND j.attempts < j.max_attempts))
          AND j.generation = 9223372036854775807
    ) OR EXISTS (
        SELECT 1 FROM public.jobs j
        WHERE j.status = 'running'
          AND NOT EXISTS (SELECT 1 FROM public.effects e WHERE e.job_id = j.id)
          AND j.generation = 9223372036854775807
    ) THEN
        RAISE EXCEPTION 'job generation cannot be advanced safely';
    END IF;
END
$preflight$;

UPDATE public.runtime_control
   SET external_sends_enabled = FALSE, updated_at = clock_timestamp()
 WHERE id = 1;

UPDATE public.effects
   SET status = 'unknown', updated_at = clock_timestamp()
 WHERE status = 'sending';

UPDATE public.jobs j
   SET status = 'unknown',
       result = jsonb_build_object('effect_id', e.id, 'status', 'unknown'),
       error_code = 'lease_lost', lease_owner = NULL, lease_until = NULL,
       updated_at = clock_timestamp()
  FROM public.effects e
 WHERE e.job_id = j.id AND e.status = 'unknown';

UPDATE public.jobs j
   SET status = 'pending',
       generation = j.generation + 1,
       result = NULL, error_code = NULL, lease_owner = NULL, lease_until = NULL,
       updated_at = clock_timestamp()
  FROM public.effects e
 WHERE e.job_id = j.id AND e.status = 'succeeded' AND j.status IN ('pending', 'running');

UPDATE public.jobs j
   SET status = CASE WHEN e.status = 'failed' THEN 'failed' ELSE 'cancelled' END,
       result = NULL,
       error_code = CASE WHEN e.status = 'failed' THEN 'execution_failed' ELSE 'cancelled' END,
       lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
  FROM public.effects e
 WHERE e.job_id = j.id AND e.status IN ('failed', 'cancelled') AND j.status IN ('pending', 'running');

UPDATE public.jobs j
   SET status = CASE
           WHEN j.deadline <= statement_timestamp() THEN 'failed'
           WHEN j.attempts >= j.max_attempts THEN 'failed'
           ELSE 'pending'
       END,
       generation = CASE
           WHEN j.deadline <= statement_timestamp() OR j.attempts >= j.max_attempts THEN j.generation
           ELSE j.generation + 1
       END,
       result = NULL,
       error_code = CASE
           WHEN j.deadline <= statement_timestamp() THEN 'deadline_expired'
           WHEN j.attempts >= j.max_attempts THEN 'attempts_exceeded'
           ELSE NULL
       END,
       lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
  FROM public.effects e
 WHERE e.job_id = j.id AND e.status = 'prepared' AND j.status IN ('pending', 'running');

UPDATE public.jobs j
   SET status = CASE
           WHEN j.deadline <= statement_timestamp() THEN 'failed'
           WHEN j.attempts >= j.max_attempts THEN 'failed'
           ELSE 'pending'
       END,
       generation = CASE
           WHEN j.deadline <= statement_timestamp() OR j.attempts >= j.max_attempts THEN j.generation
           ELSE j.generation + 1
       END,
       result = NULL,
       error_code = CASE
           WHEN j.deadline <= statement_timestamp() THEN 'deadline_expired'
           WHEN j.attempts >= j.max_attempts THEN 'attempts_exceeded'
           ELSE NULL
       END,
       lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
 WHERE j.status = 'running'
   AND NOT EXISTS (SELECT 1 FROM public.effects e WHERE e.job_id = j.id);

UPDATE public.sessions
   SET revoked_at = COALESCE(revoked_at, clock_timestamp())
 WHERE revoked_at IS NULL;
DELETE FROM public.login_flows;

DO $verify$
BEGIN
    IF (SELECT external_sends_enabled FROM public.runtime_control WHERE id = 1) IS DISTINCT FROM FALSE
       OR EXISTS (SELECT 1 FROM public.effects WHERE status = 'sending')
       OR EXISTS (SELECT 1 FROM public.jobs WHERE status = 'running')
       OR EXISTS (
           SELECT 1 FROM public.effects e JOIN public.jobs j ON j.id = e.job_id
           WHERE e.status = 'unknown'
             AND (j.status <> 'unknown'
                  OR j.result IS DISTINCT FROM jsonb_build_object('effect_id', e.id, 'status', 'unknown')
                  OR j.error_code IS DISTINCT FROM 'lease_lost'
                  OR j.lease_owner IS NOT NULL OR j.lease_until IS NOT NULL)
       )
       OR EXISTS (SELECT 1 FROM public.sessions WHERE revoked_at IS NULL)
       OR EXISTS (SELECT 1 FROM public.login_flows)
    THEN
        RAISE EXCEPTION 'post-restore safety invariant failed';
    END IF;
END
$verify$;
COMMIT;
`;
      runPsql(admin, restoreDatabase, recoverRuntimeState);
    }
    const sourceFingerprint = restoredDataFingerprint(admin, sourceDatabase);
    const targetFingerprint = restoredDataFingerprint(admin, restoreDatabase);
    const mismatchedTables = Object.keys(sourceFingerprint).filter((table) =>
      sourceFingerprint[table].rows !== targetFingerprint[table].rows ||
      sourceFingerprint[table].md5 !== targetFingerprint[table].md5);
    assert(mismatchedTables.length === 0,
      `restored archive changed durable P1 fingerprints (${mismatchedTables.join(', ')})`);
    const restoredText = runPsql(admin, restoreDatabase,
      `SELECT jsonb_build_object(` +
      `'external_sends_enabled', (SELECT external_sends_enabled FROM public.runtime_control WHERE id=1), ` +
      `'prepared_effect_status', (SELECT status FROM public.effects WHERE job_id='${preparedJobId}'::uuid), ` +
      `'prepared_reservation_status', (SELECT status FROM public.budget_reservations WHERE job_id='${preparedJobId}'::uuid), ` +
      `'prepared_job_status', (SELECT status FROM public.jobs WHERE id='${preparedJobId}'::uuid), ` +
      `'prepared_generation', (SELECT generation FROM public.jobs WHERE id='${preparedJobId}'::uuid), ` +
      `'prepared_attempts', (SELECT attempts FROM public.jobs WHERE id='${preparedJobId}'::uuid), ` +
      `'prepared_reserved_units', (SELECT units FROM public.budget_reservations WHERE job_id='${preparedJobId}'::uuid), ` +
      `'prepared_spent_units', (SELECT spent_units FROM public.project_budgets WHERE project_id=(SELECT project_id FROM public.jobs WHERE id='${preparedJobId}'::uuid)), ` +
      `'sending_effect_status', (SELECT status FROM public.effects WHERE job_id='${sendingJobId}'::uuid), ` +
      `'sending_reservation_status', (SELECT status FROM public.budget_reservations WHERE job_id='${sendingJobId}'::uuid), ` +
      `'sending_job_status', (SELECT status FROM public.jobs WHERE id='${sendingJobId}'::uuid), ` +
      `'sending_generation', (SELECT generation FROM public.jobs WHERE id='${sendingJobId}'::uuid), ` +
      `'sending_attempts', (SELECT attempts FROM public.jobs WHERE id='${sendingJobId}'::uuid), ` +
      `'sending_reserved_units', (SELECT units FROM public.budget_reservations WHERE job_id='${sendingJobId}'::uuid), ` +
      `'sending_spent_units', (SELECT spent_units FROM public.project_budgets WHERE project_id=(SELECT project_id FROM public.jobs WHERE id='${sendingJobId}'::uuid)), ` +
      `'running_jobs', (SELECT count(*) FROM public.jobs WHERE status='running'), ` +
      `'sending_effects', (SELECT count(*) FROM public.effects WHERE status='sending'), ` +
      `'active_sessions', (SELECT count(*) FROM public.sessions WHERE revoked_at IS NULL), ` +
      `'login_flows', (SELECT count(*) FROM public.login_flows)` +
      `)::text;`);
    let restored;
    try { restored = JSON.parse(restoredText); } catch { throw new Error('restored safety state was malformed'); }
    const sourcePreparedJob = persistedJob(admin, sourceDatabase, preparedJobId);
    const sourcePreparedEffect = persistedEffect(admin, sourceDatabase, preparedJobId);
    const sourceSendingJob = persistedJob(admin, sourceDatabase, sendingJobId);
    const sourceSendingEffect = persistedEffect(admin, sourceDatabase, sendingJobId);
    assert(sourcePreparedJob.status === 'running' && sourcePreparedEffect?.status === 'prepared' &&
      sourcePreparedEffect.reservationStatus === 'held' && sourcePreparedEffect.reservedUnits > 0 &&
      sourceSendingJob.status === 'running' && sourceSendingEffect?.status === 'sending' &&
      sourceSendingEffect.reservationStatus === 'held' && sourceSendingEffect.reservedUnits > 0,
    'source archive did not contain both a prepared effect and a SIGKILL-interrupted sending effect');
    assert(restored.external_sends_enabled === false &&
      restored.prepared_effect_status === 'prepared' && restored.prepared_reservation_status === 'held' &&
      restored.prepared_job_status === 'pending' && Number(restored.prepared_generation) > sourcePreparedJob.generation &&
      Number(restored.prepared_attempts) === sourcePreparedJob.attempts &&
      Number(restored.prepared_reserved_units) === sourcePreparedEffect?.reservedUnits &&
      Number(restored.prepared_reserved_units) > 0 && Number(restored.prepared_spent_units) === 0 &&
      restored.sending_effect_status === 'unknown' && restored.sending_reservation_status === 'held' &&
      restored.sending_job_status === 'unknown' && Number(restored.sending_generation) >= sourceSendingJob.generation &&
      Number(restored.sending_attempts) === sourceSendingJob.attempts &&
      Number(restored.sending_reserved_units) === sourceSendingEffect?.reservedUnits &&
      Number(restored.sending_reserved_units) > 0 && Number(restored.sending_spent_units) === 0 &&
      Number(restored.running_jobs) === 0 && Number(restored.sending_effects) === 0 &&
      Number(restored.active_sessions) === 0 && Number(restored.login_flows) === 0,
      'restored database did not preserve both prepared and sending intents while disabling sends and fencing/revoking runtime state');
    return {
      archive_sha256: createHash('sha256').update(readFileSync(archivePath)).digest('hex'),
      archive_bytes: statSync(archivePath).size,
      source_database: sourceDatabase,
      restore_database: restoreDatabase,
      restore_engine: usedOperationsCli ? 'managed PowerShell backup/restore CLI' : 'real pg_dump/pg_restore CLI',
      integrity_fingerprints_match: true,
      fingerprinted_tables: Object.keys(sourceFingerprint).length,
      fingerprint_row_counts: Object.fromEntries(Object.entries(targetFingerprint).map(([table, value]) => [table, value.rows])),
      source_prepared_job_generation: sourcePreparedJob.generation,
      restored_pending_job_generation: Number(restored.prepared_generation),
      source_sending_job_status: sourceSendingJob.status,
      source_sending_effect_status: sourceSendingEffect.status,
      restored_sending_job_status: restored.sending_job_status,
      restored_sending_effect_status: restored.sending_effect_status,
      source_sending_job_attempts: sourceSendingJob.attempts,
      restored_sending_job_attempts: Number(restored.sending_attempts),
      prepared_effect_after_restore: restored.prepared_effect_status,
      prepared_reservation_after_restore: restored.prepared_reservation_status,
      prepared_reserved_units_after_restore: Number(restored.prepared_reserved_units),
      prepared_spent_units_after_restore: Number(restored.prepared_spent_units),
      sending_effect_after_restore: restored.sending_effect_status,
      sending_reservation_after_restore: restored.sending_reservation_status,
      sending_job_after_restore: restored.sending_job_status,
      sending_job_attempts_after_restore: Number(restored.sending_attempts),
      sending_reserved_units_after_restore: Number(restored.sending_reserved_units),
      sending_spent_units_after_restore: Number(restored.sending_spent_units),
      external_sends_enabled_after_restore: false,
      running_jobs_after_restore: Number(restored.running_jobs),
      sending_effects_after_restore: Number(restored.sending_effects),
      active_sessions_after_restore: Number(restored.active_sessions),
      login_flows_after_restore: Number(restored.login_flows),
      worker_started_on_restore: false,
      worker_restart_after_restore_tested_separately: false,
      preexisting_database_modified: false,
    };
  } finally {
    rmSync(tempDirectory, { recursive: true, force: true });
  }
}

async function verifyRestoredRuntime({
  admin,
  adminUrl,
  restoreDatabase,
  preparedJobId,
  sendingJobId,
  projectId,
  preparedProjectId,
  executeProjectId,
  organizationId,
  actorId,
  preRestoreCookieHeader,
  provider,
  controlToken,
  apiKey,
  runToken,
  options,
  dockerContext,
}) {
  const apiOrigin = testApiOrigin;
  const dockerHost = options.execution === 'docker' ? 'host.docker.internal' : null;
  const databaseUrl = targetDatabaseUrl(adminUrl, 'kyro_api', restoreDatabase, null, { dockerHost });
  let api = null;
  let worker = null;
  try {
    api = await startApiProcess(databaseUrl, provider, runToken, dockerContext);
    const revokedCookie = await requestJson(`${apiOrigin}/v1/auth/session`, {
      headers: { cookie: preRestoreCookieHeader },
    });
    assert(revokedCookie.response.status === 401,
      'pre-restore opaque session cookie remained valid against the restored database');

    const restoredSession = await login(apiOrigin, provider, controlToken, 'synthetic-user-a');
    assert(restoredSession.actorId === actorId,
      'synthetic OIDC login after restore did not resolve the persisted actor');
    const organizations = await apiRequest(apiOrigin, '/v1/organizations', restoredSession);
    assert(organizations.response.status === 200 &&
      organizations.data?.organizations?.some((item) => item.organization_id === organizationId),
    'restored actor did not retain organization membership');
    const memberships = await apiRequest(apiOrigin, `/v1/organizations/${organizationId}/members`, restoredSession);
    assert(memberships.response.status === 200 &&
      memberships.data?.memberships?.some((item) => item.actor_id === actorId && item.role === 'owner'),
    'restored organization owner membership was not available after a fresh login');
    const project = await apiRequest(apiOrigin, `/v1/projects/${projectId}`, restoredSession);
    assert(project.response.status === 200 && project.data?.project?.id === projectId,
      'restored sending-fixture project grant did not authorize a fresh read');
    const preparedProject = await apiRequest(apiOrigin, `/v1/projects/${preparedProjectId}`, restoredSession);
    assert(preparedProject.response.status === 200 && preparedProject.data?.project?.id === preparedProjectId,
      'restored prepared-fixture project grant did not authorize a fresh read');
    const executeProject = await apiRequest(apiOrigin, `/v1/projects/${executeProjectId}`, restoredSession);
    assert(executeProject.response.status === 200,
      'persisted Execute project was not readable after fresh login');
    const executeRevisionBefore = Number(executeProject.data.project.current_revision);
    const executeAdmission = await enqueueJob(apiOrigin, executeProjectId, restoredSession,
      { kind: 'apply_changes', changes: { operations: [
        { op: 'set_preference', key: 'restore.execute.persisted', value: true },
      ] } }, executeRevisionBefore, `e2e-restore-persisted-execute-${runToken}`);
    assert(executeAdmission.response.status === 202 && executeAdmission.data?.status === 'pending',
      'restored actor could not use its persisted Execute grant to admit an ApplyChanges job');

    const sendsEnabled = runPsql(admin, restoreDatabase,
      'SELECT external_sends_enabled FROM public.runtime_control WHERE id=1;');
    const preparedBeforeWorker = persistedJob(admin, restoreDatabase, preparedJobId);
    const preparedEffectBeforeWorker = persistedEffect(admin, restoreDatabase, preparedJobId);
    const sendingBeforeWorker = persistedJob(admin, restoreDatabase, sendingJobId);
    const sendingEffectBeforeWorker = persistedEffect(admin, restoreDatabase, sendingJobId);
    assert(sendsEnabled === 'f' && preparedBeforeWorker.status === 'pending' &&
      preparedEffectBeforeWorker?.status === 'prepared' && preparedEffectBeforeWorker.reservationStatus === 'held' &&
      sendingBeforeWorker.status === 'unknown' && sendingEffectBeforeWorker?.status === 'unknown' &&
      sendingEffectBeforeWorker.reservationStatus === 'held',
    'restored safety state changed before runtime restart validation');
    const providerCallsBeforeWorker = (await providerSnapshot(provider)).inferenceRequests;
    const workerApplication = `kyro_e2e_${runToken}_restore`;
    const workerUrl = targetDatabaseUrl(adminUrl, 'kyro_worker', restoreDatabase, workerApplication, { dockerHost });
    worker = startWorkerProcess(workerUrl, apiKey, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    await waitUntil(() => serviceIsRunning(worker),
      { timeoutMs: 3000, intervalMs: 50, label: 'restored worker process start' });
    await waitForWorkerIdleConnection(admin, restoreDatabase, workerApplication);

    const executeJobTerminal = await waitForJob(apiOrigin, executeProjectId,
      executeAdmission.data.id, restoredSession, ['succeeded'], 20_000);
    const executeProjectAfterResult = await apiRequest(apiOrigin,
      `/v1/projects/${executeProjectId}`, restoredSession);
    assert(executeProjectAfterResult.response.status === 200,
      'restored project was not readable after ApplyChanges worker completion');
    const executeRevisionAfter = Number(executeProjectAfterResult.data.project.current_revision);
    const executeRevisionResult = await apiRequest(apiOrigin,
      `/v1/projects/${executeProjectId}/revisions/${executeRevisionAfter}`, restoredSession);
    assertOpenApiComponent('AppRevision', executeRevisionResult.data);
    assert(executeRevisionResult.response.status === 200 && executeJobTerminal.result?.revision === executeRevisionAfter &&
      executeRevisionAfter === executeRevisionBefore + 1 &&
      executeRevisionResult.data.spec?.preferences?.['restore.execute.persisted'] === true,
    'restored ApplyChanges job did not exercise Write and persist the expected preference/revision');

    const reconciliation = await reconcileEffect(apiOrigin, projectId, sendingEffectBeforeWorker.effectId,
      restoredSession, `synthetic-restored-not-processed-${runToken}`, { outcome: 'not_processed' },
      `e2e-restore-resume-reconcile-${runToken}`);
    assert(reconciliation.response.status === 202,
      'same-project local reconciliation was not admitted while restored outbound sends were disabled');
    const localTerminal = await waitForJob(apiOrigin, projectId, reconciliation.data.id, restoredSession,
      ['succeeded', 'failed', 'stale', 'cancelled'], 20_000);
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 750));
    await waitForWorkerIdleConnection(admin, restoreDatabase, workerApplication);

    const preparedAfterWorker = persistedJob(admin, restoreDatabase, preparedJobId);
    const preparedEffectAfterWorker = persistedEffect(admin, restoreDatabase, preparedJobId);
    const sendingAfterWorker = persistedJob(admin, restoreDatabase, sendingJobId);
    const sendingEffectAfterWorker = persistedEffect(admin, restoreDatabase, sendingJobId);
    const sendsAfterWorker = runPsql(admin, restoreDatabase,
      'SELECT external_sends_enabled FROM public.runtime_control WHERE id=1;');
    const providerCallsAfterWorker = (await providerSnapshot(provider)).inferenceRequests;
    assert(localTerminal.status === 'succeeded' && sendsAfterWorker === 'f' &&
      preparedAfterWorker.status === preparedBeforeWorker.status &&
      preparedAfterWorker.generation === preparedBeforeWorker.generation &&
      preparedAfterWorker.attempts === preparedBeforeWorker.attempts &&
      preparedEffectAfterWorker?.status === 'prepared' && preparedEffectAfterWorker.reservationStatus === 'held' &&
      sendingAfterWorker.status === 'failed' && sendingAfterWorker.generation === sendingBeforeWorker.generation &&
      sendingAfterWorker.attempts === sendingBeforeWorker.attempts &&
      sendingEffectAfterWorker?.status === 'failed' && sendingEffectAfterWorker.reservationStatus === 'released' &&
      providerCallsAfterWorker === providerCallsBeforeWorker,
    'restored worker replayed a ModelCall, changed its attempt/generation, or failed same-project reconciliation under the durable pause');

    const logout = await apiRequest(apiOrigin, '/v1/auth/logout', restoredSession, { method: 'POST', body: {} });
    assert(logout.response.status === 204, 'fresh restored session could not be revoked after verification');
    const afterLogout = await apiRequest(apiOrigin, '/v1/auth/session', restoredSession);
    assert(afterLogout.response.status === 401, 'fresh restored session remained active after logout');
    const activeSessionsAfterLogout = Number(runPsql(admin, restoreDatabase,
      'SELECT count(*) FROM public.sessions WHERE revoked_at IS NULL;'));
    assert(activeSessionsAfterLogout === 0,
      'fresh restored session logout did not leave the restored database without active sessions');
    const workerStop = await stopChild(worker, 'SIGTERM');
    worker = null;
    assert(workerStop?.code === 0, 'restored worker did not stop cleanly after the paused-send check');
    return {
      old_cookie_rejected_http_status: revokedCookie.response.status,
      fresh_oidc_login_succeeded: true,
      persisted_actor_and_owner_membership_resolved: true,
      persisted_project_read_authorized: true,
      persisted_execute_grant_authorized: executeAdmission.response.status === 202,
      persisted_apply_changes_job_status: executeJobTerminal.status,
      persisted_apply_changes_revision_before_after: [executeRevisionBefore, executeRevisionAfter],
      persisted_apply_changes_preference_committed: true,
      restored_worker_started: true,
      restored_worker_role: 'kyro_worker',
      restored_worker_idle_observations: 2,
      local_job_terminal_status: localTerminal.status,
      local_job_kind: 'ReconcileEffect/not_processed',
      paused_prepared_model_status_after_resume: preparedAfterWorker.status,
      paused_prepared_model_attempts_before_after: [preparedBeforeWorker.attempts, preparedAfterWorker.attempts],
      interrupted_sending_model_status_after_resume: sendingAfterWorker.status,
      interrupted_sending_model_attempts_before_after: [sendingBeforeWorker.attempts, sendingAfterWorker.attempts],
      external_sends_enabled_after_worker_start: sendsAfterWorker === 't',
      provider_call_delta_after_worker_start: providerCallsAfterWorker - providerCallsBeforeWorker,
      fresh_session_logout_http_status: logout.response.status,
      fresh_session_rejected_after_logout_http_status: afterLogout.response.status,
      active_sessions_after_logout: activeSessionsAfterLogout,
      worker_shutdown: 'SIGTERM',
      worker_exit_code: workerStop.code,
    };
  } finally {
    await stopChild(worker, 'SIGTERM').catch(() => {});
    await stopChild(api, 'SIGTERM').catch(() => {});
  }
}

function getAdminMetadata(options) {
  return parseAdminUrl(options.adminUrl);
}

async function runLocalPreflight(options) {
  const admin = getAdminMetadata(options);
  const tools = {};
  for (const executable of ['psql', 'pg_dump', 'pg_restore']) {
    const probe = command(executable, ['--version'], { allowFailure: true, timeout: 5000 });
    if (probe.status !== 0) throw new Error(`local execution requires ${executable} in PATH`);
    tools[executable] = probe.stdout.trim().split(/\s+/).slice(0, 2).join(' ');
  }
  const rows = runPsql(admin, admin.database,
    "SELECT current_user, current_database(), current_setting('server_version'), " +
    "(SELECT rolsuper FROM pg_roles WHERE rolname=current_user), " +
    "(SELECT rolbypassrls FROM pg_roles WHERE rolname=current_user), " +
    "(SELECT rolcreaterole FROM pg_roles WHERE rolname=current_user), " +
    "(SELECT rolcreatedb FROM pg_roles WHERE rolname=current_user);");
  const [user, database, version, superuser, bypassRls, createRole, createDb] = rows.split('|');
  if (user !== admin.user || database !== admin.database || !version ||
      !((superuser === 't' || bypassRls === 't') && (superuser === 't' || createRole === 't') && createDb === 't')) {
    throw new Error('local PostgreSQL did not match the configured loopback administrative role');
  }
  const roles = runPsql(admin, admin.database,
    "SELECT rolname, rolsuper, rolbypassrls FROM pg_roles WHERE rolname LIKE 'kyro_%' ORDER BY 1;")
    .split(/\r?\n/).filter(Boolean).map((line) => {
      const [role, superRole, bypassRls] = line.split('|');
      return { role, superuser: superRole === 't', bypass_rls: bypassRls === 't' };
    });
  const portsAvailable = {};
  for (const port of [testApiPort, testProviderPort, testControlPort]) portsAvailable[String(port)] = await checkLocalPortFree(port);
  const report = {
    record: 'part1-e2e-preflight',
    status: 'passed_read_only',
    execution: 'local',
    versions: { node: process.version, postgresql: version, ...tools },
    database_resource: {
      host_is_loopback: true,
      port: admin.port,
      admin_role_matches_url: true,
      maintenance_database_matches_url: true,
      admin_superuser: superuser === 't',
      admin_bypasses_rls: bypassRls === 't',
      admin_can_create_roles: superuser === 't' || createRole === 't',
      admin_can_create_database: createDb === 't',
    },
    runtime_roles_observed_in_maintenance_database: roles,
    expected_loopback_ports_available: portsAvailable,
    database_created: false,
    existing_database_modified: false,
    persistent_containers_created: 0,
    real_secrets_used: false,
    acceptance_criteria_verified: [],
  };
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  if (Object.values(portsAvailable).some((available) => !available)) {
    process.stderr.write('One or more E2E loopback ports are occupied; --run must not start until the selected API port, 9090 and 9091 are free.\n');
    return 2;
  }
  return 0;
}

function assertRunArtifacts() {
  const requiredPaths = [
    'Cargo.lock',
    'Cargo.toml',
    'tests/fixtures/models.synthetic.e2e.json',
    'crates/kyro-api/src/main.rs',
    'crates/kyro-worker/src/main.rs',
    'crates/kyro-store/migrations/0001_foundation.sql',
    'docs/backend/partie-1/openapi.v1.json',
    'scripts/check-openapi.mjs',
    'scripts/synthetic-provider.mjs',
  ];
  const missing = requiredPaths.filter((path) => !existsSync(resolve(repoRoot, path)));
  if (missing.length) {
    throw new Error(`integration is not ready; missing ${missing.join(', ')}`);
  }
  const acceptance = readFileSync(resolve(repoRoot, 'docs/backend/partie-1/ACCEPTATION.md'), 'utf8');
  if (!['P1-01', 'P1-14'].every((criterion) => acceptance.includes(criterion))) {
    throw new Error('the protected P1 acceptance contract was not found');
  }
}

async function runAcceptance(options) {
  const runToken = randomBytes(8).toString('hex');
  const runId = `${new Date().toISOString().replace(/[-:.]/g, '').replace('T', 'T').slice(0, 15)}-${runToken}`;
  const report = {
    record: 'part1-e2e-run',
    run_id: runId,
    started_at: new Date().toISOString(),
    status: 'running',
    execution: options.execution,
    source: captureSourceState(),
    versions: { node: process.version },
    database_name_prefix: 'kyro_p1_e2e_',
    services: { postgres: 'real', api: 'real binary', worker: 'real binary', oidc: 'synthetic mock', inference: 'synthetic mock' },
    migrations: { concurrent_runs: [], replay_exit_code: null },
    database_created: false,
    existing_database_modified: false,
    real_secrets_used: false,
    measured_provider_requests: 'unknown',
    measured_model_usage: 'unknown until returned by the synthetic provider and persisted ledger',
    criteria: Object.fromEntries(Array.from({ length: 14 }, (_, index) => [`P1-${String(index + 1).padStart(2, '0')}`, { status: 'not_run' }])),
    failure: null,
  };
  let currentCriterion = null;
  let admin = null;
  let database = null;
  let restoreDatabase = null;
  let provider = null;
  let apiProcess = null;
  let workerProcess = null;
  let sentinelLogCheck = null;
  const additionalWorkerProcesses = [];
  let dockerContext = null;
  try {
    if (options.sourceSha256 !== null && options.sourceSha256 !== report.source.sha256) {
      throw new Error('requested source fingerprint does not match the current build/test inputs');
    }
    if (!report.source.worktree_clean && options.sourceSha256 === null) {
      throw new Error('uncommitted acceptance runs require --source-sha256 from --fingerprint');
    }
    const preflight = await runPreflight(options);
    if (preflight !== 0) throw new Error('PostgreSQL/provider preflight did not pass');
    const adminUrl = options.adminUrl ?? 'postgresql://kyro_admin@127.0.0.1:55440/kyro_p1?sslmode=disable';
    admin = executionAdmin(options);
    if (options.execution === 'docker') {
      report.versions.docker_server = docker(['version', '--format', '{{.Server.Version}}']).stdout.trim();
      report.versions.postgresql = runPsql(admin, admin.database, 'SHOW server_version;');
    }
    assertRunArtifacts();

    const openApiCheck = await runChild(process.execPath, [resolve(repoRoot, 'scripts/check-openapi.mjs')], {
      env: cleanEnvironment(), timeout: 60_000,
    });
    report.open_api_validation = {
      command: 'node scripts/check-openapi.mjs',
      node: process.version,
      response_schema_validator: 'runner JSON Schema subset, OpenAPI 3.1 components',
      exit_code: openApiCheck.code,
    };
    if (openApiCheck.code !== 0) throw new Error('OpenAPI route and schema validation failed');

    currentCriterion = 'P1-01';
    if (options.execution === 'local') {
      const build = await runChild('cargo', ['build', '--locked', '--workspace', '--bins'], {
        env: cleanEnvironment(), timeout: 600_000,
      });
      report.build = { command: 'cargo build --locked --workspace --bins', exit_code: build.code, signal: build.signal ?? null };
      if (build.code !== 0) throw new Error('locked workspace binary build failed');
      for (const name of ['kyro-api', 'kyro-worker', 'kyro-migrate']) {
        if (!existsSync(runtimeBinary(name))) throw new Error(`required executable ${name} was not built`);
      }
    } else {
      dockerContext = { appImage: buildDockerAppImage(runToken) };
      const imageId = docker(['image', 'inspect', '--format', '{{.Id}}', dockerContext.appImage]).stdout.trim();
      assert(/^sha256:[a-f0-9]{64}$/.test(imageId), 'built runtime image did not expose an immutable image id');
      dockerContext.appImageId = imageId;
      report.build = {
        command: 'docker build -f Dockerfile .',
        exit_code: 0,
        image_pinned_runtime: true,
        image_id: imageId,
      };
    }

    database = await createTestDatabase(admin, runToken);
    report.database_created = true;
    const targetAdminUrl = options.execution === 'docker'
      ? dockerAdminUrl(adminUrl, database)
      : targetDatabaseUrl(adminUrl, admin.user, database);
    await runMigrations(targetAdminUrl, report, dockerContext);
    await verifyRuntimeRoleAndRls(admin, database, adminUrl, report);
    report.criteria['P1-01'] = {
      status: 'partial',
      checks: ['locked binaries built', 'two concurrent migrations succeeded', 'migration replay succeeded', 'real PostgreSQL schema applied', 'API/worker roles are non-admin with forced RLS'],
      pending: ['API readiness', 'worker connected and stably idle under its runtime role'],
    };

    const key = randomBytes(32).toString('base64url');
    const controlToken = randomBytes(32).toString('base64url');
    report.secret_sentinel_included_in_test_input = false;
    provider = options.execution === 'docker'
      ? await startDockerProvider({ apiKey: key, controlToken, runToken })
      : await startSyntheticProvider({
        host: '127.0.0.1', port: testProviderPort, publicHost: '127.0.0.1',
        controlHost: '127.0.0.1', controlPort: testControlPort,
        clientId: 'kyro-e2e-client', defaultSubject: 'synthetic-user-a',
        controlToken, expectedApiKey: key,
      });
    if (dockerContext) dockerContext.providerContainer = provider.container;
    const dockerHost = options.execution === 'docker' ? 'host.docker.internal' : null;
    const apiDatabaseUrl = targetDatabaseUrl(adminUrl, 'kyro_api', database, null, { dockerHost });
    const workerDatabaseUrl = targetDatabaseUrl(adminUrl, 'kyro_worker', database, null, { dockerHost });
    apiProcess = await startApiProcess(apiDatabaseUrl, provider, runToken, dockerContext);
    report.api_readiness = {
      status: 'ready',
      bind: dockerContext ? `0.0.0.0:${testApiPort} (container; loopback-published)` : `127.0.0.1:${testApiPort}`,
      runtime: options.execution,
      ...(dockerContext ? {
        absolute_entrypoint: apiProcess.dockerEntrypoint,
        host_windows_path_forwarded: apiProcess.hostWindowsPathForwarded,
      } : {}),
    };

    const readinessWorkerApplication = `kyro_e2e_${runToken}_readiness`;
    const readinessWorkerUrl = targetDatabaseUrl(adminUrl, 'kyro_worker', database, readinessWorkerApplication, {
      dockerHost: options.execution === 'docker' ? 'host.docker.internal' : null,
    });
    workerProcess = startWorkerProcess(readinessWorkerUrl, key, runToken, {
      pollMs: 25, leaseSeconds: 10, dockerContext,
    });
    await waitUntil(() => serviceIsRunning(workerProcess),
      { timeoutMs: 3000, intervalMs: 50, label: 'worker readiness process start' });
    await waitForWorkerIdleConnection(admin, database, readinessWorkerApplication);
    const readinessWorkerStop = await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    assert(readinessWorkerStop?.code === 0,
      'readiness worker did not stop cleanly after its stable idle check');
    report.worker_readiness = {
      status: 'connected_idle_stable',
      role: 'kyro_worker',
      idle_observations: 2,
      shutdown: 'SIGTERM',
      exit_code: readinessWorkerStop.code,
      ...(dockerContext ? {
        absolute_entrypoint: '/usr/local/bin/kyro-worker',
        host_windows_path_forwarded: false,
      } : {}),
    };
    const p101Checks = [
      'locked binaries built',
      'two concurrent migrations succeeded',
      'migration replay succeeded',
      'real PostgreSQL schema applied',
      'API ready over HTTP',
      'worker connected as kyro_worker and remained idle for two observations',
      'API/worker roles are non-admin with forced RLS',
    ];
    if (dockerContext) {
      assert(apiProcess.dockerEntrypoint === '/usr/local/bin/kyro-api' &&
        apiProcess.hostWindowsPathForwarded === false,
      'API Docker configuration did not use its absolute entrypoint or received a Windows host path');
      assert(readinessWorkerStop && readinessWorkerStop.code === 0,
        'readiness worker did not stop cleanly after its stable idle check');
      p101Checks.push('Docker runtime used absolute API/worker entrypoints without forwarding a Windows host PATH');
    }
    report.criteria['P1-01'] = {
      status: 'passed',
      checks: p101Checks,
    };

    currentCriterion = 'P1-02';
    for (const claims of [
      { aud: 'synthetic-wrong-audience' },
      { iss: 'http://127.0.0.1:9090/wrong-issuer' },
      { nonce: 'synthetic-wrong-nonce' },
    ]) {
      const rejected = await login(testApiOrigin, provider, controlToken, 'synthetic-user-invalid', claims);
      assert(rejected.rejected, 'OIDC callback accepted a forbidden claim');
    }
    const actorA = await login(testApiOrigin, provider, controlToken, 'synthetic-user-a');
    const noCsrf = await apiRequest(testApiOrigin, '/v1/organizations', actorA, {
      method: 'POST', body: { name: 'Synthetic no-CSRF' }, noCsrf: true,
    });
    assert([400, 401, 403].includes(noCsrf.response.status), 'mutation accepted without CSRF token');
    const wrongOrigin = await apiRequest(testApiOrigin, '/v1/organizations', actorA, {
      method: 'POST', headers: { origin: 'http://127.0.0.1:8081' }, body: { name: 'Synthetic wrong-origin' },
    });
    assert([400, 401, 403].includes(wrongOrigin.response.status), 'mutation accepted from a different Origin');
    const replayUrl = new URL('/v1/auth/callback', testApiOrigin);
    replayUrl.searchParams.set('code', actorA.callbackCode);
    replayUrl.searchParams.set('state', actorA.callbackState);
    const replayCallback = await requestJson(replayUrl, { headers: { cookie: `kyro_oidc_binding=${actorA.browserBinding}` } });
    assert(!cookieFrom(replayCallback.response.headers, 'kyro_session') && [400, 401, 403].includes(replayCallback.response.status),
      'OIDC callback code/state replay created another session');
    const refresh = await apiRequest(testApiOrigin, '/v1/auth/refresh', actorA, { method: 'POST', body: {} });
    assert(refresh.response.ok, 'authenticated session refresh failed');
    const refreshedSession = {
      actorId: actorA.actorId,
      csrf: cookieFrom(refresh.response.headers, 'kyro_csrf'),
      sessionCookie: cookieFrom(refresh.response.headers, 'kyro_session'),
    };
    assert(refreshedSession.csrf && refreshedSession.sessionCookie,
      'session refresh did not rotate opaque session and CSRF cookies');
    refreshedSession.cookieHeader = `kyro_session=${refreshedSession.sessionCookie}; kyro_csrf=${refreshedSession.csrf}`;
    const oldSession = await apiRequest(testApiOrigin, '/v1/auth/session', actorA);
    const newSession = await apiRequest(testApiOrigin, '/v1/auth/session', refreshedSession);
    assert(oldSession.response.status === 401 && newSession.response.status === 200,
      'session refresh did not revoke the previous opaque session');
    const actorB = await login(testApiOrigin, provider, controlToken, 'synthetic-user-b');
    assert(actorA.actorId !== actorB.actorId, 'different synthetic OIDC subjects resolved to one actor');
    const logoutActor = await login(testApiOrigin, provider, controlToken, 'synthetic-user-logout');
    const logout = await apiRequest(testApiOrigin, '/v1/auth/logout', logoutActor, { method: 'POST', body: {} });
    assert(logout.response.status === 204, 'authenticated logout did not revoke the server-side session');
    const afterLogout = await apiRequest(testApiOrigin, '/v1/auth/session', logoutActor);
    assert(afterLogout.response.status === 401, 'logged-out opaque session remained usable');
    const expiringActor = await login(testApiOrigin, provider, controlToken, 'synthetic-user-expiring');
    assert(/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(expiringActor.actorId),
      'synthetic actor identifier is not a UUID');
    const expiredSessions = runPsql(admin, database,
      `UPDATE public.sessions SET created_at=clock_timestamp()-interval '2 seconds', ` +
      `expires_at=clock_timestamp()-interval '1 second' WHERE actor_id='${expiringActor.actorId}'::uuid ` +
      `AND revoked_at IS NULL RETURNING id;`).split(/\r?\n/).filter(Boolean);
    assert(expiredSessions.length === 1, 'expected exactly one persisted session for the expiry fixture');
    const afterExpiry = await apiRequest(testApiOrigin, '/v1/auth/session', expiringActor);
    assert(afterExpiry.response.status === 401, 'expired opaque session remained usable');
    report.identity = {
      distinct_actors: true,
      bad_claims_rejected: 3,
      csrf_refused: true,
      origin_refused: true,
      refreshed_session_rotated: true,
      callback_replay_refused: true,
      logout_revoked: true,
      expired_session_refused: true,
    };
    report.criteria['P1-02'] = {
      status: 'passed',
      checks: [
        'RS256 synthetic OIDC success', 'bad issuer/audience/nonce refused', 'opaque cookie session',
        'CSRF/Origin refusal', 'callback replay refusal', 'refresh revoked prior session',
        'logout revoked persisted session', 'database-expired session refused',
      ],
    };

    currentCriterion = 'P1-03';
    const orgA = await createOrganization(testApiOrigin, refreshedSession, 'Synthetic E2E Org A');
    const orgB = await createOrganization(testApiOrigin, actorB, 'Synthetic E2E Org B');
    const projectA = await createProject(testApiOrigin, refreshedSession, orgA.id, 'Synthetic E2E Project A');
    const projectB = await createProject(testApiOrigin, actorB, orgB.id, 'Synthetic E2E Project B');
    const paginationProject = await createProject(testApiOrigin, refreshedSession, orgA.id, 'Synthetic Pagination Project');
    const firstProjects = await apiRequest(testApiOrigin, '/v1/projects?limit=1', refreshedSession);
    assert(firstProjects.response.status === 200 && firstProjects.data.length === 1,
      'project pagination did not retain the bounded array response');
    const projectCursor = firstProjects.response.headers.get('x-next-cursor');
    assert(projectCursor, 'project pagination silently omitted the next page');
    const secondProjects = await apiRequest(testApiOrigin,
      `/v1/projects?limit=1&before=${encodeURIComponent(projectCursor)}`, refreshedSession);
    assert(secondProjects.response.status === 200 && secondProjects.data.length === 1 &&
      !secondProjects.response.headers.has('x-next-cursor') &&
      new Set([...firstProjects.data, ...secondProjects.data].map((project) => project.id)).size === 2 &&
      [...firstProjects.data, ...secondProjects.data].every((project) => [projectA.id, paginationProject.id].includes(project.id)),
    'project pagination lost, duplicated, or exposed another actor project');
    const foreignCursorProjects = await apiRequest(testApiOrigin,
      `/v1/projects?limit=1&before=${encodeURIComponent(projectCursor)}`, actorB);
    assert(foreignCursorProjects.response.status === 200 &&
      foreignCursorProjects.data.every((project) => project.id === projectB.id),
    'a foreign project cursor conferred visibility');
    for (const query of ['limit=0', 'limit=1001', 'before=not-base64', `before=${'x'.repeat(513)}`]) {
      const invalid = await apiRequest(testApiOrigin, `/v1/projects?${query}`, refreshedSession);
      assert(invalid.response.status === 400, 'invalid project pagination was not rejected');
    }
    report.project_pagination = { pages: 2, array_body_preserved: true, no_duplicates_or_foreign_projects: true, invalid_queries_refused: 4 };
    const projectBSeed = await applyChange(testApiOrigin, projectB.id, actorB, 0,
      'e2e-project-b-valid-revision', [
        { op: 'set_preference', key: 'owner.seed', value: true },
      ]);
    const projectBSeedRevision = Number(projectBSeed.data?.revision?.revision ?? projectBSeed.data?.revision);
    assert([200, 201].includes(projectBSeed.response.status) && projectBSeedRevision === 1,
      'project B owner could not create the revision needed for valid cross-actor mutation requests');
    const crossPaths = [
      `/v1/projects/${projectB.id}`,
      `/v1/projects/${projectB.id}/revisions/0`,
      `/v1/projects/${projectB.id}/jobs`,
      `/v1/projects/${projectB.id}/budget`,
      `/v1/projects/${projectB.id}/effects`,
      `/v1/projects/${projectB.id}/events`,
    ];
    const crossStatuses = [];
    for (const path of crossPaths) {
      const result = await apiRequest(testApiOrigin, path, refreshedSession);
      crossStatuses.push(result.response.status);
      assert([403, 404].includes(result.response.status), `cross-project read was not denied: ${path}`);
    }
    const projectBBeforeMutations = await apiRequest(testApiOrigin,
      `/v1/projects/${projectB.id}`, actorB);
    assert(projectBBeforeMutations.response.status === 200,
      'project B owner could not read its project before cross-actor mutation checks');
    const projectBBefore = parseProject(projectBBeforeMutations.data);
    const projectBRevisionBefore = Number(projectBBefore.current_revision);
    const projectBEventSequenceBefore = Number(projectBBefore.event_sequence);
    const readProjectBMutationCounts = () => ({
      jobs: Number(runPsql(admin, database,
        `SELECT count(*) FROM public.jobs WHERE project_id='${projectB.id}'::uuid;`)),
      change_commands: Number(runPsql(admin, database,
        `SELECT count(*) FROM public.change_commands WHERE project_id='${projectB.id}'::uuid;`)),
      events: Number(runPsql(admin, database,
        `SELECT count(*) FROM public.events WHERE project_id='${projectB.id}'::uuid;`)),
      outbox_events: Number(runPsql(admin, database,
        `SELECT count(*) FROM public.outbox_events WHERE project_id='${projectB.id}'::uuid;`)),
    });
    const projectBCountsBefore = readProjectBMutationCounts();
    const crossChange = await applyChange(testApiOrigin, projectB.id, refreshedSession,
      projectBRevisionBefore, 'e2e-cross-project-change-denied', [
        { op: 'set_preference', key: 'cross-project-denied', value: true },
      ]);
    const crossJob = await enqueueJob(testApiOrigin, projectB.id, refreshedSession,
      { kind: 'apply_changes', changes: { operations: [{
        op: 'set_preference', key: 'cross-project-job-denied', value: true,
      }] } }, projectBRevisionBefore, 'e2e-cross-project-job-denied');
    const crossMutationStatuses = [crossChange.response.status, crossJob.response.status];
    assert(crossMutationStatuses.every((status) => [403, 404].includes(status)),
      'cross-project ChangeSet or ApplyChanges admission was not denied');
    const projectBAfterMutationsResult = await apiRequest(testApiOrigin,
      `/v1/projects/${projectB.id}`, actorB);
    assert(projectBAfterMutationsResult.response.status === 200,
      'project B owner could not read its project after cross-actor mutation checks');
    const projectBAfter = parseProject(projectBAfterMutationsResult.data);
    const projectBCountsAfter = readProjectBMutationCounts();
    assert(Number(projectBAfter.current_revision) === projectBRevisionBefore &&
      Number(projectBAfter.event_sequence) === projectBEventSequenceBefore &&
      isDeepStrictEqual(projectBCountsAfter, projectBCountsBefore),
    'denied cross-project mutations changed project revision, jobs, events, outbox, or change commands');
    const rlsBaseline = runPsql(admin, database,
      "SELECT count(*) FROM public.projects;");
    assert(Number(rlsBaseline) >= 2, 'fixture did not persist both synthetic projects');
    const rlsApiRows = runPsql(admin, database,
      "SELECT count(*) FROM public.projects;", { user: 'kyro_api' });
    assert(rlsApiRows === '0', 'runtime API role saw project rows without actor context');
    report.cross_project = {
      read_routes_refused: crossStatuses.length,
      all_read_routes_refused: true,
      project_b_owner_seed_revision: projectBSeedRevision,
      mutation_attempts_refused: crossMutationStatuses.length,
      mutation_http_statuses: crossMutationStatuses,
      project_b_revision_before_after: [projectBRevisionBefore, Number(projectBAfter.current_revision)],
      project_b_event_sequence_before_after: [projectBEventSequenceBefore, Number(projectBAfter.event_sequence)],
      project_b_counts_before: projectBCountsBefore,
      project_b_counts_after: projectBCountsAfter,
      no_cross_mutation_persisted: true,
      runtime_unscoped_project_rows: Number(rlsApiRows),
      context_cleared_after_commit: true,
    };
    report.criteria['P1-03'] = { status: 'passed', checks: [
      'two real identities/projects isolated across six HTTP read surfaces',
      'valid cross-project ChangeSet and ApplyChanges admission refused with CSRF, Origin, If-Match, and idempotency headers',
      'denied mutation left project revision, event sequence, jobs, change commands, events, and outbox unchanged',
      'non-owner runtime role sees zero unscoped rows',
      'FORCE ROW LEVEL SECURITY verified after project fixture creation',
    ] };

    currentCriterion = 'P1-04';
    const initial = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}`, refreshedSession);
    assertOpenApiComponent('ProjectSnapshot', initial.data);
    assert(initial.response.status === 200 && initial.data.revision.revision === 0, 'project did not start at immutable revision zero');
    const addNodeOperations = [
      { op: 'add_node', node: { id: 'e2e-node-a', kind: 'layout.panel', properties: { title: 'Synthetic E2E' } } },
    ];
    assertOpenApiComponent('ChangeSet', { operations: addNodeOperations });
    const addNode = await applyChange(testApiOrigin, projectA.id, refreshedSession, 0, 'e2e-rev-add-node', addNodeOperations);
    assert(addNode.response.status === 200 || addNode.response.status === 201, 'valid ChangeSet was refused');
    const revisionOne = addNode.data.revision?.revision ?? addNode.data.revision;
    assert(Number(revisionOne) === 1, 'ChangeSet did not create exactly one new revision');
    const historical = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}/revisions/0`, refreshedSession);
    const current = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}/revisions/1`, refreshedSession);
    assertOpenApiComponent('AppRevision', historical.data);
    assertOpenApiComponent('AppRevision', current.data);
    assert(historical.response.status === 200 && historical.data.spec.nodes.length === 0,
      'revision zero was mutated by a later ChangeSet');
    assert(current.response.status === 200 && current.data.spec.nodes[0]?.id === 'e2e-node-a',
      'new immutable revision omitted the added synthetic node');
    const stale = await applyChange(testApiOrigin, projectA.id, refreshedSession, 0, 'e2e-stale-change', [
      { op: 'set_property', node_id: 'e2e-node-a', key: 'title', value: 'stale' },
    ]);
    const missingMatch = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}/changes`, refreshedSession, {
      method: 'POST', headers: { 'idempotency-key': 'e2e-missing-ifmatch' },
      body: { operations: [{ op: 'set_preference', key: 'missing', value: true }] },
    });
    const unknown = await applyChange(testApiOrigin, projectA.id, refreshedSession, 1, 'e2e-unknown-op', [
      { op: 'run_shell', command: 'must-not-run' },
    ]);
    const oversized = await applyChange(testApiOrigin, projectA.id, refreshedSession, 1, 'e2e-too-many-ops',
      Array.from({ length: 129 }, (_, index) => ({ op: 'set_preference', key: `e2e-${index}`, value: true })));
    assert(stale.response.status === 412 && missingMatch.response.status === 428 &&
      [400, 413].includes(unknown.response.status) && [400, 413].includes(oversized.response.status),
      'stale/missing/unknown/oversized ChangeSets did not receive the expected refusal');
    report.revisions = { revision_zero_immutable: true, current_revision: Number(revisionOne), stale_refused: true, missing_if_match_refused: true, unknown_operation_refused: true, oversized_operation_count_refused: true };
    report.criteria['P1-04'] = { status: 'passed', checks: ['revision 0 preserved', 'revision 1 contains only the declared node', 'stale CAS 412', 'missing If-Match 428', 'unknown and over-limit operations refused'] };

    currentCriterion = 'P1-05';
    const beforeIdempotent = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}`, refreshedSession);
    const beforeSequence = beforeIdempotent.data.project.event_sequence;
    const repeatedOps = [{ op: 'set_preference', key: 'e2e-idempotent', value: 'same' }];
    const repeated = await Promise.all(Array.from({ length: 8 }, () => applyChange(
      testApiOrigin, projectA.id, refreshedSession, 1, 'e2e-concurrent-idempotency', repeatedOps,
    )));
    assert(repeated.every((item) => item.response.status === 200 || item.response.status === 201),
      'same-fingerprint concurrent ChangeSet was not idempotently accepted');
    const stableCommand = JSON.stringify(repeated[0].data);
    assert(repeated.every((item) => JSON.stringify(item.data) === stableCommand),
      'idempotent ChangeSet replays did not return one stable result');
    const mismatched = await applyChange(testApiOrigin, projectA.id, refreshedSession, 1,
      'e2e-concurrent-idempotency', [{ op: 'set_preference', key: 'e2e-idempotent', value: 'different' }]);
    const afterIdempotent = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}`, refreshedSession);
    assert(mismatched.response.status === 409 && afterIdempotent.data.project.current_revision === 2 &&
      afterIdempotent.data.project.event_sequence === beforeSequence + 1,
      'idempotency key mismatch or replay produced an extra revision/event');
    report.idempotency = { concurrent_replays: repeated.length, one_stable_result: true, fingerprint_mismatch_status: 409, revision_delta: 1, event_sequence_delta: 1 };
    report.criteria['P1-05'] = { status: 'passed', checks: ['8 simultaneous same-key/same-fingerprint requests returned one result', 'same key with changed fingerprint refused 409', 'one revision and one event appended'] };

    currentCriterion = 'P1-06';
    const workerApplication = `kyro_e2e_${runToken}_worker`;
    const workerUrl = targetDatabaseUrl(adminUrl, 'kyro_worker', database, workerApplication, {
      dockerHost: options.execution === 'docker' ? 'host.docker.internal' : null,
    });
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 2, dockerContext });
    await waitUntil(() => serviceIsRunning(workerProcess),
      { timeoutMs: 3000, intervalMs: 50, label: 'worker process start' });
    const idleStop = await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    const durableProject = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}`, refreshedSession);
    const durableRevision = durableProject.data.project.current_revision;
    const durableJobResult = await enqueueJob(testApiOrigin, projectA.id, refreshedSession,
      { kind: 'apply_changes', changes: { operations: [{
        op: 'add_node', node: { id: 'e2e-worker-durable', kind: 'text.heading', properties: { text: 'durable' } },
      }] } }, durableRevision, 'e2e-worker-durable-job');
    const durableErrorCode = durableJobResult.data?.error?.code ?? 'none';
    const durableResponseStatus = durableJobResult.data?.status ?? 'absent';
    assert(durableJobResult.response.status === 202 && durableJobResult.data.status === 'pending',
      `job was not durably pending while its worker was stopped ` +
      `(http_status=${durableJobResult.response.status}, error_code=${durableErrorCode}, response_status=${durableResponseStatus})`);
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 2, dockerContext });
    const durableDone = await waitForJob(testApiOrigin, projectA.id, durableJobResult.data.id,
      refreshedSession, ['succeeded'], 20_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;

    const crashProject = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}`, refreshedSession);
    const crashRevision = crashProject.data.project.current_revision;
    const crashJobResponse = await enqueueJob(testApiOrigin, projectA.id, refreshedSession,
      { kind: 'apply_changes', changes: { operations: [{
        op: 'add_node', node: { id: 'e2e-worker-crash-recovery', kind: 'text.heading', properties: { text: 'recover' } },
      }] } }, crashRevision, 'e2e-worker-crash-recovery');
    assert(crashJobResponse.response.status === 202, 'crash-recovery job was not admitted');
    const revisionTrigger = {
      functionName: `e2e_block_revision_${runToken}`,
      triggerName: `e2e_block_revision_${runToken}`,
      table: 'app_revisions',
      predicate: `NEW.project_id='${projectA.id}'::uuid AND NEW.revision=${crashRevision + 1} AND TG_OP='INSERT'`,
    };
    installBlockTrigger(admin, database, revisionTrigger);
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 2, dockerContext });
    await waitForWorkerSleep(admin, database, workerApplication);
    const beforeKill = persistedJob(admin, database, crashJobResponse.data.id);
    assert(beforeKill.status === 'running' && beforeKill.generation >= 1, 'worker had not claimed the trigger-blocked job');
    await stopChild(workerProcess, 'SIGKILL');
    workerProcess = null;
    removeBlockTrigger(admin, database, revisionTrigger);
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 2500));
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 2, dockerContext });
    const recovered = await waitForJob(testApiOrigin, projectA.id, crashJobResponse.data.id,
      refreshedSession, ['succeeded'], 25_000);
    assert(recovered.generation > beforeKill.generation,
      'reclaimed worker did not advance the fencing generation');
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    report.worker_recovery = {
      idle_worker_stopped_before_admission: idleStop.signal === 'SIGTERM',
      pending_job_persisted_while_stopped: true,
      pending_job_succeeded_after_restart: durableDone.status === 'succeeded',
      kill_during_internal_apply_signal: 'SIGKILL',
      generation_before_crash: beforeKill.generation,
      generation_after_reclaim: recovered.generation,
      recovered_status: recovered.status,
    };
    report.criteria['P1-06'] = { status: 'passed', checks: ['job remains pending while worker stopped', 'worker restart claims and completes durable job', 'SIGKILL during transactional internal apply rolls back and reclaims with higher generation'] };

    currentCriterion = 'P1-07';
    const sourceBeforeCall = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}`, refreshedSession);
    const sourceRevision = sourceBeforeCall.data.project.current_revision;
    const sourceJobResponse = await enqueueJob(testApiOrigin, projectA.id, refreshedSession,
      { kind: 'model_call', request: syntheticModelRequest('Synthetic stale-source sentinel.') },
      sourceRevision, 'e2e-late-source-result');
    assert(sourceJobResponse.response.status === 202, 'source-fencing model job was not admitted');
    const sourceCallsBefore = (await providerSnapshot(provider)).inferenceRequests;
    await configureProviderScenario(provider, { mode: 'hold' });
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    await waitForProviderCount(provider, sourceCallsBefore + 1);
    const sourceMutation = await applyChange(testApiOrigin, projectA.id, refreshedSession,
      sourceRevision, 'e2e-mutate-during-model-call', [
        { op: 'set_preference', key: 'late-result-source', value: true },
      ]);
    assert(sourceMutation.response.status === 200 || sourceMutation.response.status === 201,
      'source revision could not advance while an inference request was held');
    await releaseProviderInference(provider);
    const sourceJob = await waitForPersistedJob(admin, database, sourceJobResponse.data.id,
      ['stale', 'failed', 'succeeded'], 20_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    const sourceEffect = persistedEffect(admin, database, sourceJobResponse.data.id);
    const sourceLedgerCount = countForJob(admin, database, 'usage_ledger', sourceJobResponse.data.id);
    const sourceEvents = assertJobLifecycleEvents(
      lifecycleEventsForJob(admin, database, projectA.id, sourceJobResponse.data.id), 'job.stale', 'stale source model job');
    assert(sourceJob.status === 'stale' && sourceEffect?.reservationStatus === 'settled' &&
      sourceEffect.reservedUnits > 0 && sourceLedgerCount === 1,
      'late result after a source revision change was not suppressed while accounting settled');

    const membership = await apiRequest(testApiOrigin, `/v1/organizations/${orgA.id}/members`, refreshedSession, {
      method: 'POST', body: { actor_id: actorB.actorId, role: 'member' },
    });
    assert(membership.response.status === 204, 'second synthetic actor was not added to the project organization');
    const grantResponse = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}/grants`, refreshedSession, {
      method: 'POST', body: {
        actor_id: actorB.actorId,
        actions: ['read', 'execute', 'model', 'budget'],
        resources: ['*'],
      },
    });
    assert(grantResponse.response.status === 201 && grantResponse.data?.id,
      'owner could not grant synthetic actor scoped project permissions');
    const sourceAfterLate = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}`, refreshedSession);
    const grantJobResponse = await enqueueJob(testApiOrigin, projectA.id, actorB,
      { kind: 'model_call', request: syntheticModelRequest('Synthetic revoked-grant sentinel.') },
      sourceAfterLate.data.project.current_revision, 'e2e-late-grant-result');
    assert(grantJobResponse.response.status === 202, 'granted actor could not enqueue its model job');
    const grantCallsBefore = (await providerSnapshot(provider)).inferenceRequests;
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    await waitForProviderCount(provider, grantCallsBefore + 1);
    const revoke = await apiRequest(testApiOrigin,
      `/v1/projects/${projectA.id}/grants/${grantResponse.data.id}`, refreshedSession, { method: 'DELETE' });
    assert(revoke.response.status === 204, 'project owner could not revoke the synthetic model grant');
    await releaseProviderInference(provider);
    const grantJob = await waitForPersistedJob(admin, database, grantJobResponse.data.id,
      ['stale', 'failed', 'succeeded'], 20_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    const grantEffect = persistedEffect(admin, database, grantJobResponse.data.id);
    const grantLedgerCount = countForJob(admin, database, 'usage_ledger', grantJobResponse.data.id);
    const grantEvents = assertJobLifecycleEvents(
      lifecycleEventsForJob(admin, database, projectA.id, grantJobResponse.data.id), 'job.stale', 'revoked grant model job');
    assert(grantJob.status !== 'succeeded' && grantEffect?.reservationStatus === 'settled' &&
      grantEffect.reservedUnits > 0 && grantLedgerCount === 1,
      'late result after grant revocation was accepted or accounting was lost');
    report.late_results = {
      source_change_status: sourceJob.status,
      source_change_reservation: sourceEffect.reservationStatus,
      source_change_ledger_rows: sourceLedgerCount,
      grant_revocation_status: grantJob.status,
      grant_revocation_reservation: grantEffect.reservationStatus,
      grant_revocation_ledger_rows: grantLedgerCount,
      source_lifecycle_events: sourceEvents,
      grant_lifecycle_events: grantEvents,
      provider_calls: 2,
    };
    report.criteria['P1-07'] = { status: 'passed', checks: ['source revision advanced while synthetic inference was blocked', 'late source result did not succeed job but settlement ledger persisted', 'project grant revoked while second inference was blocked; result suppressed and accounting persisted'] };

    currentCriterion = 'P1-08';
    const boundedLimits = {
      ...syntheticProjectLimits,
      max_active_jobs: 1,
      max_queued_jobs: 1,
      max_job_attempts: 1,
      job_ttl_secs: 15,
    };
    const boundedProject = await createProject(testApiOrigin, refreshedSession,
      orgA.id, 'Synthetic bounded queue', { limits: boundedLimits });
    const badBounds = await Promise.all([
      enqueueJob(testApiOrigin, boundedProject.id, refreshedSession,
        { kind: 'apply_changes', changes: { operations: [] } }, 0, 'e2e-attempts-zero', { max_attempts: 0 }),
      enqueueJob(testApiOrigin, boundedProject.id, refreshedSession,
        { kind: 'apply_changes', changes: { operations: [] } }, 0, 'e2e-attempts-high', { max_attempts: 4 }),
      enqueueJob(testApiOrigin, boundedProject.id, refreshedSession,
        { kind: 'apply_changes', changes: { operations: [] } }, 0, 'e2e-ttl-zero', { ttl_seconds: 0 }),
      enqueueJob(testApiOrigin, boundedProject.id, refreshedSession,
        { kind: 'apply_changes', changes: { operations: [] } }, 0, 'e2e-ttl-high', { ttl_seconds: 1801 }),
    ]);
    assert(badBounds.every((item) => [400, 413, 422].includes(item.response.status) ||
      (item.response.status === 429 && item.data?.error?.code === 'resource_limit')) &&
      runPsql(admin, database, `SELECT count(*) FROM jobs WHERE project_id='${boundedProject.id}'::uuid;`) === '0',
      'out-of-range job attempt or TTL settings were admitted');
    const pendingForCancel = await enqueueJob(testApiOrigin, boundedProject.id, refreshedSession,
      { kind: 'apply_changes', changes: { operations: [{ op: 'set_preference', key: 'queued', value: true }] } },
      0, 'e2e-pending-cancel', { max_attempts: 1, ttl_seconds: 15 });
    assert(pendingForCancel.response.status === 202 && pendingForCancel.data.status === 'pending',
      'bounded project did not persist one pending job');
    const saturated = await enqueueJob(testApiOrigin, boundedProject.id, refreshedSession,
      { kind: 'apply_changes', changes: { operations: [{ op: 'set_preference', key: 'overflow', value: true }] } },
      0, 'e2e-queue-overflow');
    assert([409, 429].includes(saturated.response.status), 'queue admission exceeded the project queued-job limit');
    const cancelledPending = await apiRequest(testApiOrigin,
      `/v1/projects/${boundedProject.id}/jobs/${pendingForCancel.data.id}`, refreshedSession, { method: 'DELETE' });
    assert(cancelledPending.response.status === 202 && cancelledPending.data.status === 'pending' && cancelledPending.data.cancel_requested === true,
      'pending job cancellation did not persist the cancellation request');
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    const pendingCancelledTerminal = await waitForJob(testApiOrigin, boundedProject.id,
      pendingForCancel.data.id, refreshedSession, ['cancelled'], 20_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    assert(Number(runPsql(admin, database, `SELECT current_revision FROM projects WHERE id='${boundedProject.id}'::uuid;`)) === 0,
      'cancelled pending ApplyChanges mutated the project');
    const modelCallsBeforeCancel = (await providerSnapshot(provider)).inferenceRequests;
    const pendingModel = await enqueueJob(testApiOrigin, boundedProject.id, refreshedSession,
      { kind: 'model_call', request: syntheticModelRequest('Synthetic pending cancellation.') },
      0, 'e2e-pending-model-cancel', { max_attempts: 1, ttl_seconds: 15 });
    assert(pendingModel.response.status === 202, 'pending model job was not admitted after a queue slot freed');
    const cancelPendingModel = await apiRequest(testApiOrigin,
      `/v1/projects/${boundedProject.id}/jobs/${pendingModel.data.id}`, refreshedSession, { method: 'DELETE' });
    assert(cancelPendingModel.response.status === 202 && cancelPendingModel.data.status === 'pending' && cancelPendingModel.data.cancel_requested === true,
      'pending model cancellation request was not persisted before dispatch');
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    await waitForJob(testApiOrigin, boundedProject.id, pendingModel.data.id, refreshedSession, ['cancelled'], 20_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 300));
    assert((await providerSnapshot(provider)).inferenceRequests === modelCallsBeforeCancel,
      'cancelled pending model job caused a provider request');

    const oversizedBody = await apiRequest(testApiOrigin, `/v1/projects/${boundedProject.id}/jobs`, refreshedSession, {
      method: 'POST', headers: { 'if-match': '"rev-0"', 'idempotency-key': 'e2e-http-body-oversize' },
      body: { payload: { kind: 'model_call', request: syntheticModelRequest('x'.repeat(270_000)) } },
    });
    const invalidDeadline = await enqueueJob(testApiOrigin, boundedProject.id, refreshedSession,
      { kind: 'model_call', request: { ...syntheticModelRequest(), deadline_ms: 10_001 } },
      0, 'e2e-deadline-over-policy');
    assert([413, 400].includes(oversizedBody.response.status) &&
      ([400, 413, 422].includes(invalidDeadline.response.status) ||
        (invalidDeadline.response.status === 429 && invalidDeadline.data?.error?.code === 'resource_limit')),
      'oversized HTTP input or out-of-policy model deadline was admitted');

    const beforeRunningCancel = (await providerSnapshot(provider)).inferenceRequests;
    await configureProviderScenario(provider, { mode: 'hold' });
    const runningCandidate = await enqueueJob(testApiOrigin, boundedProject.id, refreshedSession,
      { kind: 'model_call', request: syntheticModelRequest('Synthetic running cancellation.') },
      0, 'e2e-running-cancel', { max_attempts: 1, ttl_seconds: 15 });
    assert(runningCandidate.response.status === 202, 'running-cancellation model job was not admitted');
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    await waitForProviderCount(provider, beforeRunningCancel + 1);
    const cancelRunning = await apiRequest(testApiOrigin,
      `/v1/projects/${boundedProject.id}/jobs/${runningCandidate.data.id}`, refreshedSession, { method: 'DELETE' });
    assert(cancelRunning.response.status === 202 && cancelRunning.data.status === 'running' && cancelRunning.data.cancel_requested === true,
      'running cancellation did not persist cancel_requested');
    await releaseProviderInference(provider);
    const cancelledRunning = await waitForJob(testApiOrigin, boundedProject.id,
      runningCandidate.data.id, refreshedSession, ['cancelled', 'unknown', 'failed', 'succeeded'], 20_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 500));
    assert(cancelledRunning.status === 'cancelled' &&
      (await providerSnapshot(provider)).inferenceRequests === beforeRunningCancel + 1,
      'cancelled running work integrated a result or was automatically re-sent');
    const runningCancelCalls = (await providerSnapshot(provider)).inferenceRequests - beforeRunningCancel;
    const beforeDeadlineCall = (await providerSnapshot(provider)).inferenceRequests;
    await configureProviderScenario(provider, { mode: 'success', delayMs: 500 });
    const deadlineJobResponse = await enqueueJob(testApiOrigin, boundedProject.id, refreshedSession,
      { kind: 'model_call', request: { ...syntheticModelRequest('Synthetic bounded deadline.'), deadline_ms: 100 } },
      0, 'e2e-deadline-expiration', { max_attempts: 1, ttl_seconds: 15 });
    assert(deadlineJobResponse.response.status === 202, 'bounded model deadline job was not admitted');
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    const deadlineJob = await waitForJob(testApiOrigin, boundedProject.id,
      deadlineJobResponse.data.id, refreshedSession, ['unknown', 'failed', 'succeeded'], 15_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    assert(deadlineJob.status !== 'succeeded' &&
      (await providerSnapshot(provider)).inferenceRequests === beforeDeadlineCall + 1,
      'expired model deadline produced business success or an automatic retry');
    report.bounds_and_cancellation = {
      invalid_attempt_and_ttl_requests_refused: badBounds.length,
      project_queue_saturation_refused: [409, 429].includes(saturated.response.status),
      pending_cancellation_terminal: pendingCancelledTerminal.status,
      pending_model_provider_calls: 0,
      oversized_body_status: oversizedBody.response.status,
      invalid_deadline_status: invalidDeadline.response.status,
      running_cancel_terminal: cancelledRunning.status,
      running_cancel_provider_calls: runningCancelCalls,
      deadline_terminal: deadlineJob.status,
      deadline_provider_calls: (await providerSnapshot(provider)).inferenceRequests - beforeDeadlineCall,
    };
    report.criteria['P1-08'] = { status: 'passed', checks: ['attempt/TTL and request-body bounds refused', 'queued-job saturation enforced', 'pending cancellation freed capacity without provider call', 'running cancellation suppressed business result with a single provider call'] };

    currentCriterion = 'P1-09';
    const concurrentRequest = syntheticModelRequest('Concurrent synthetic budget probe.');
    // The reservation covers the exact provider envelope and its structured-output schema.
    const fixtureModel = JSON.parse(readFileSync(resolve(repoRoot, 'tests/fixtures/models.synthetic.e2e.json'), 'utf8')).destinations[0].models[0];
    const envelope = {
      model: concurrentRequest.model,
      messages: [{ role: 'user', content: JSON.stringify(concurrentRequest.input) }],
      max_tokens: concurrentRequest.max_output_tokens, n: 1, stream: false, store: false,
      response_format: { type: 'json_schema', json_schema: {
        name: fixtureModel.output_schema.id, strict: true, schema: {
          type: 'object', properties: {
            schema_id: { type: 'string', const: fixtureModel.output_schema.id, maxLength: 128 },
            schema_version: { type: 'string', const: fixtureModel.output_schema.version, maxLength: 128 },
            data: fixtureModel.output_schema.schema,
          }, required: ['schema_id', 'schema_version', 'data'], additionalProperties: false,
        },
      } },
    };
    const perCallReserve = Buffer.byteLength(JSON.stringify(envelope), 'utf8') + 64 + concurrentRequest.max_output_tokens;
    const nearLimit = await createProject(testApiOrigin, refreshedSession,
      orgA.id, 'Synthetic concurrent budget', { budgetLimitUnits: 2 * perCallReserve });
    await setBudgetLimit(testApiOrigin, nearLimit.id, refreshedSession, 2 * perCallReserve);
    const budgetJobs = await Promise.all(Array.from({ length: 6 }, (_, index) => enqueueJob(
      testApiOrigin, nearLimit.id, refreshedSession,
      { kind: 'model_call', request: concurrentRequest },
      0, `e2e-near-limit-${index}`, { max_attempts: 1, ttl_seconds: 60 },
    )));
    assert(budgetJobs.every((item) => item.response.status === 202),
      'concurrent budget probe jobs were not all durably admitted before execution');
    const beforeNearLimit = (await providerSnapshot(provider)).inferenceRequests;
    await configureProviderScenario(provider, { mode: 'hold' });
    const budgetWorkers = [];
    for (let index = 0; index < 4; index += 1) {
      const workerUrl = targetDatabaseUrl(adminUrl, 'kyro_worker', database,
        `kyro_e2e_${runToken}_budget_${index}`, { dockerHost });
      const worker = startWorkerProcess(workerUrl, key, runToken,
        { pollMs: 25, leaseSeconds: 10, dockerContext });
      budgetWorkers.push(worker);
      additionalWorkerProcesses.push(worker);
    }
    await waitUntil(() => heldReservationUnits(admin, database, nearLimit.id) === 2 * perCallReserve,
      { timeoutMs: 12_000, intervalMs: 50, label: 'two atomic model-budget reservations near the cap' });
    const heldProviderCalls = await waitForProviderCount(provider, beforeNearLimit + 2);
    assert(heldProviderCalls.inferenceRequests === beforeNearLimit + 2,
      'model reservations beyond the exact two-call budget cap reached the provider');
    const duringBudget = budgetCounters(admin, database, nearLimit.id);
    assert(duringBudget.reservedUnits <= duringBudget.limitUnits - duringBudget.spentUnits &&
      duringBudget.reservedUnits === 2 * perCallReserve,
      'concurrent held reservations exceeded the configured project budget');
    await waitUntil(() => runPsql(admin, database,
      `SELECT count(*) FROM jobs WHERE project_id='${nearLimit.id}'::uuid AND status='failed';`) === '4',
      { timeoutMs: 12_000, intervalMs: 50, label: 'four over-budget jobs refused before holds release' });
    await releaseProviderInference(provider);
    const budgetJobViews = await Promise.all(budgetJobs.map((item) => waitForJob(
      testApiOrigin, nearLimit.id, item.data.id, refreshedSession,
      ['succeeded', 'failed', 'unknown', 'cancelled', 'stale'], 25_000,
    )));
    for (const worker of budgetWorkers) await stopChild(worker, 'SIGTERM');
    additionalWorkerProcesses.length = 0;
    const afterBudget = budgetCounters(admin, database, nearLimit.id);
    const succeededBudgetJobs = budgetJobs.filter((item, index) => budgetJobViews[index].status === 'succeeded');
    const succeededBudgetEvents = succeededBudgetJobs.map((item) => assertJobLifecycleEvents(
      lifecycleEventsForJob(admin, database, nearLimit.id, item.data.id), 'job.succeeded', 'settled model job'));
    const ledgerRowsByJob = succeededBudgetJobs.map((item) => countForJob(admin, database, 'usage_ledger', item.data.id));
    assert(budgetJobViews.filter((job) => job.status === 'succeeded').length === 2 &&
      succeededBudgetJobs.every((item, index) => ledgerRowsByJob[index] === 1) &&
      afterBudget.reservedUnits === 0 && afterBudget.spentUnits === 56 &&
      Number(runPsql(admin, database, `SELECT COALESCE(sum(units),0) FROM usage_ledger WHERE project_id='${nearLimit.id}'::uuid;`)) === afterBudget.spentUnits &&
      afterBudget.spentUnits <= afterBudget.limitUnits &&
      (await providerSnapshot(provider)).inferenceRequests === beforeNearLimit + 2,
      'near-limit concurrent spending exceeded budget or settled a provider result more than once');
    report.concurrent_budget = {
      limit_units: afterBudget.limitUnits,
      measured_per_call_reservation_units: perCallReserve,
      provider_calls_while_held: heldProviderCalls.inferenceRequests - beforeNearLimit,
      job_terminal_statuses: budgetJobViews.map((job) => job.status),
      final_reserved_units: afterBudget.reservedUnits,
      final_spent_units: afterBudget.spentUnits,
      ledger_rows_for_succeeded_jobs: ledgerRowsByJob,
      committed_lifecycle_event_sets: succeededBudgetEvents,
    };
    report.criteria['P1-09'] = { status: 'passed', checks: ['six admitted model jobs competed through four PostgreSQL workers', 'atomic reservation never exceeded two-call budget', 'exactly two provider calls and two usage ledger rows settled', 'event rows used committed job lifecycle types'] };

    currentCriterion = 'P1-12';
    const eventProject = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}`, refreshedSession);
    const latestSequence = eventProject.data.project.event_sequence;
    const unauthorizedSse = await startSse(testApiOrigin, projectA.id, actorB,
      `${projectA.id}:${latestSequence}`);
    assert([403, 404].includes(unauthorizedSse.status), 'revoked actor opened a project event stream');
    const malformedSse = await requestJson(`${testApiOrigin}/v1/projects/${projectA.id}/events?after=invalid`, {
      headers: { cookie: refreshedSession.cookieHeader },
    });
    const foreignSse = await startSse(testApiOrigin, projectA.id, refreshedSession, `${projectB.id}:1`);
    const futureSse = await startSse(testApiOrigin, projectA.id, refreshedSession, `${projectA.id}:${latestSequence + 100}`);
    const mismatchSse = await startSse(testApiOrigin, projectA.id, refreshedSession, `${projectA.id}:0`, {
      'last-event-id': `${projectA.id}:1`,
    });
    assert(malformedSse.response.status === 400 && foreignSse.status === 409 &&
      futureSse.status === 409 && mismatchSse.status === 400,
      'SSE malformed, foreign, future, or disagreeing cursors were not refused');
    const replay = await startSse(testApiOrigin, projectA.id, refreshedSession, `${projectA.id}:0`);
    assert(replay.status === 200 && replay.headers.get('content-type')?.startsWith('text/event-stream'),
      'authorized event replay did not open an SSE stream');
    const replayEvent = await readSseEvent(replay);
    assert(replayEvent.includes('event: project-event') && replayEvent.includes(`id: ${projectA.id}:1`),
      'SSE replay did not begin at the project event cursor');

    const eventGapProject = await createProject(testApiOrigin, refreshedSession,
      orgA.id, 'Synthetic SSE environment gap');
    const gapSeedChange = await applyChange(testApiOrigin, eventGapProject.id,
      refreshedSession, eventGapProject.current_revision, 'e2e-sse-gap-seed-revision', [
        { op: 'add_node', node: { id: 'e2e-sse-seed', kind: 'text.heading', properties: { text: 'synthetic event seed' } } },
      ]);
    assert([200, 201].includes(gapSeedChange.response.status),
      'could not create the revision needed for the hidden production job fixture');
    const visibleBaseline = await apiRequest(testApiOrigin,
      `/v1/projects/${eventGapProject.id}`, refreshedSession);
    assert(visibleBaseline.response.status === 200, 'SSE gap fixture project snapshot was unreadable');
    const initialEventSequence = visibleBaseline.data.project.event_sequence;
    assert(initialEventSequence >= 1, 'SSE gap fixture did not persist its initial visible event');
    const hiddenJobIds = runPsql(admin, database,
      `INSERT INTO public.jobs (project_id, actor_id, environment, source_revision, payload, status, deadline, error_code) ` +
      `SELECT p.id, '${refreshedSession.actorId}'::uuid, 'production', p.current_revision, ` +
      `'{"kind":"synthetic_environment_fixture"}'::jsonb, 'failed', clock_timestamp()+interval '1 hour', 'execution_failed' ` +
      `FROM public.projects p WHERE p.id='${eventGapProject.id}'::uuid RETURNING id;`)
      .split(/\r?\n/).filter(Boolean);
    assert(hiddenJobIds.length === 1, 'could not persist the synthetic cross-environment job reference');
    const hiddenSequenceRows = runPsql(admin, database,
      `WITH next_event AS (UPDATE public.projects SET event_sequence=event_sequence+1 ` +
      `WHERE id='${eventGapProject.id}'::uuid RETURNING event_sequence) ` +
      `INSERT INTO public.events (project_id, sequence, type, payload, actor_id) ` +
      `SELECT '${eventGapProject.id}'::uuid, event_sequence, 'job.synthetic.hidden_environment', ` +
      `jsonb_build_object('job_id','${hiddenJobIds[0]}'::uuid), '${refreshedSession.actorId}'::uuid ` +
      `FROM next_event RETURNING sequence;`).split(/\r?\n/).filter(Boolean);
    assert(hiddenSequenceRows.length === 1, 'could not persist the synthetic hidden-environment event');
    const hiddenSequence = Number(hiddenSequenceRows[0]);
    const gapSnapshot = await apiRequest(testApiOrigin, `/v1/projects/${eventGapProject.id}`, refreshedSession);
    assert(gapSnapshot.response.status === 200 && gapSnapshot.data.project.event_sequence === hiddenSequence,
      'project snapshot did not expose the global event sequence including the other environment');
    const hiddenOnlyAfterVisible = await startSse(testApiOrigin, eventGapProject.id,
      refreshedSession, `${eventGapProject.id}:${initialEventSequence}`);
    assert(hiddenOnlyAfterVisible.status === 200 &&
      (await readSseEventOrTimeout(hiddenOnlyAfterVisible, 700)) === null,
      'SSE exposed an event from another runtime environment');
    const hiddenOnlyAfterSnapshot = await startSse(testApiOrigin, eventGapProject.id,
      refreshedSession, `${eventGapProject.id}:${hiddenSequence}`);
    assert(hiddenOnlyAfterSnapshot.status === 200 &&
      (await readSseEventOrTimeout(hiddenOnlyAfterSnapshot, 700)) === null,
      'global snapshot cursor caused a hidden-only event to be replayed');
    const visibleSequenceRows = runPsql(admin, database,
      `WITH next_event AS (UPDATE public.projects SET event_sequence=event_sequence+1 ` +
      `WHERE id='${eventGapProject.id}'::uuid RETURNING event_sequence) ` +
      `INSERT INTO public.events (project_id, sequence, type, payload, actor_id) ` +
      `SELECT '${eventGapProject.id}'::uuid, event_sequence, 'project.synthetic.visible_after_gap', ` +
      `'{"status":"synthetic-visible"}'::jsonb, '${refreshedSession.actorId}'::uuid FROM next_event RETURNING sequence;`)
      .split(/\r?\n/).filter(Boolean);
    assert(visibleSequenceRows.length === 1, 'could not persist the synthetic visible event after the environment gap');
    const visibleSequence = Number(visibleSequenceRows[0]);
    const visibleAfterGap = await startSse(testApiOrigin, eventGapProject.id,
      refreshedSession, `${eventGapProject.id}:${initialEventSequence}`);
    const visibleGapEvent = await readSseEvent(visibleAfterGap);
    assert(visibleGapEvent.includes(`id: ${eventGapProject.id}:${visibleSequence}`) &&
      !visibleGapEvent.includes('reset-required') && !visibleGapEvent.includes(hiddenJobIds[0]),
      'SSE did not skip the environment-hidden sequence and continue with the next visible event');
    const afterSnapshotGap = await apiRequest(testApiOrigin, `/v1/projects/${eventGapProject.id}`, refreshedSession);
    assert(afterSnapshotGap.data.project.event_sequence === visibleSequence,
      'snapshot event sequence did not advance across the filtered event gap');
    const prunedVisiblePrefix = runPsql(admin, database,
      `DELETE FROM public.events WHERE project_id='${eventGapProject.id}'::uuid ` +
      `AND sequence<=${initialEventSequence} RETURNING sequence;`).split(/\r?\n/).filter(Boolean);
    assert(prunedVisiblePrefix.length === initialEventSequence, 'SSE retention fixture did not prune its complete physical prefix');
    const expiredHistory = await startSse(testApiOrigin, eventGapProject.id,
      refreshedSession, `${eventGapProject.id}:0`);
    assert(expiredHistory.status === 410,
      'SSE did not reject a cursor whose visible event prefix was genuinely pruned');
    const resumable = await fetch(`${testApiOrigin}/v1/projects/${projectA.id}/events`, {
      headers: { cookie: refreshedSession.cookieHeader, 'last-event-id': `${projectA.id}:0` },
      redirect: 'manual', signal: AbortSignal.timeout(30_000),
    });
    const resumedEvent = await readSseEvent(resumable);
    assert(resumedEvent.includes(`id: ${projectA.id}:1`), 'Last-Event-ID did not replay the next event');
    const grantForStream = await apiRequest(testApiOrigin, `/v1/projects/${projectA.id}/grants`, refreshedSession, {
      method: 'POST', body: { actor_id: actorB.actorId, actions: ['read'], resources: ['*'] },
    });
    assert(grantForStream.response.status === 201, 'owner could not create temporary read grant for SSE revocation');
    const streamCursor = eventProject.data.project.event_sequence;
    const activeStream = await startSse(testApiOrigin, projectA.id, actorB, `${projectA.id}:${streamCursor}`);
    assert(activeStream.status === 200, 'granted actor could not open SSE before revocation');
    const streamRevoke = await apiRequest(testApiOrigin,
      `/v1/projects/${projectA.id}/grants/${grantForStream.data.id}`, refreshedSession, { method: 'DELETE' });
    assert(streamRevoke.response.status === 204, 'owner could not revoke active SSE reader');
    const streamClosed = await waitSseClosed(activeStream, 12_000);
    assert(streamClosed, 'SSE stream remained open after its project grant was revoked');
    report.sse = {
      replayed_from_zero: true,
      last_event_id_resumed: true,
      malformed_cursor_refused: true,
      foreign_and_future_cursors_conflict: true,
      disagreeing_cursors_refused: true,
      other_environment_event_hidden: true,
      global_snapshot_cursor_includes_hidden_sequence: true,
      event_gap_replayed_without_reset: true,
      pruned_visible_prefix_returned_gone: true,
      revoked_stream_closed: true,
    };
    report.criteria['P1-12'] = {
      status: 'passed',
      checks: [
        'persisted SSE replay and Last-Event-ID', 'malformed/future/foreign/mismatched cursor refusals',
        'other-environment sequence hidden while global snapshot cursor advanced',
        'subsequent visible event streamed across filtered sequence gap',
        'genuinely pruned visible prefix returned 410', 'active stream closed after project grant revocation',
      ],
    };

    currentCriterion = 'P1-10';
    await configureProviderScenario(provider, { mode: 'success' });
    const knownProject = await createProject(testApiOrigin, refreshedSession,
      orgA.id, 'Synthetic known-settlement recovery');
    const knownJobResponse = await enqueueJob(testApiOrigin, knownProject.id, refreshedSession,
      { kind: 'model_call', request: syntheticModelRequest('Synthetic known provider settlement.') },
      0, 'e2e-known-settlement-before-finish', { max_attempts: 1, ttl_seconds: 60 });
    assert(knownJobResponse.response.status === 202, 'known-settlement model job was not durably admitted');
    const beforeKnownCall = (await providerSnapshot(provider)).inferenceRequests;
    const knownFinishTrigger = {
      functionName: `e2e_block_known_finish_${runToken}`,
      triggerName: `e2e_block_known_finish_${runToken}`,
      table: 'jobs',
      predicate: `NEW.id='${knownJobResponse.data.id}'::uuid AND NEW.status='succeeded'`,
    };
    installBlockTrigger(admin, database, knownFinishTrigger);
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 2, dockerContext });
    await waitForProviderCount(provider, beforeKnownCall + 1);
    await waitForWorkerSleep(admin, database, workerApplication);
    const settlementWhileBlocked = persistedEffect(admin, database, knownJobResponse.data.id);
    const settlementLedgerBeforeCrash = countForJob(admin, database, 'usage_ledger', knownJobResponse.data.id);
    const settlementJobBeforeCrash = persistedJob(admin, database, knownJobResponse.data.id);
    assert(settlementWhileBlocked?.status === 'succeeded' && settlementWhileBlocked.reservationStatus === 'settled' &&
      settlementWhileBlocked.reservedUnits > 0 && settlementLedgerBeforeCrash === 1 && settlementJobBeforeCrash.status === 'running',
      'provider settlement was not durably committed before the terminal job write was blocked');
    await stopChild(workerProcess, 'SIGKILL');
    workerProcess = null;
    removeBlockTrigger(admin, database, knownFinishTrigger);
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 2500));
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 2, dockerContext });
    const knownJob = await waitForJob(testApiOrigin, knownProject.id,
      knownJobResponse.data.id, refreshedSession, ['succeeded'], 20_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    const settledEffectAfterRecovery = persistedEffect(admin, database, knownJobResponse.data.id);
    const settledLedgerAfterRecovery = countForJob(admin, database, 'usage_ledger', knownJobResponse.data.id);
    const knownLifecycle = assertJobLifecycleEvents(
      lifecycleEventsForJob(admin, database, knownProject.id, knownJobResponse.data.id), 'job.succeeded', 'known settled recovery');
    assert(knownJob.status === 'succeeded' && settledEffectAfterRecovery?.status === 'succeeded' &&
      settledEffectAfterRecovery.reservationStatus === 'settled' && settledLedgerAfterRecovery === 1 &&
      (await providerSnapshot(provider)).inferenceRequests === beforeKnownCall + 1,
      'known provider settlement was replayed, double-accounted, or not finalized after worker restart');

    const unknownProject = await createProject(testApiOrigin, refreshedSession,
      orgA.id, 'Synthetic unknown-send reconciliation');
    const budgetGrant = await apiRequest(testApiOrigin, `/v1/projects/${unknownProject.id}/grants`, refreshedSession, {
      method: 'POST', body: { actor_id: actorB.actorId, actions: ['budget'], resources: ['*'] },
    });
    assert(budgetGrant.response.status === 201, 'owner could not grant the second operator budget-only reconciliation authority');
    const unknownJobResponse = await enqueueJob(testApiOrigin, unknownProject.id, refreshedSession,
      { kind: 'model_call', request: syntheticModelRequest('Synthetic interrupted outbound request.') },
      0, 'e2e-interrupted-sending', { max_attempts: 1, ttl_seconds: 60 });
    assert(unknownJobResponse.response.status === 202, 'unknown-send model job was not durably admitted');
    const settledEffectCalls = (await providerSnapshot(provider)).inferenceRequests - beforeKnownCall;
    const beforeUnknownCall = (await providerSnapshot(provider)).inferenceRequests;
    await configureProviderScenario(provider, { mode: 'hold' });
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 2, dockerContext });
    await waitForProviderCount(provider, beforeUnknownCall + 1);
    const sendingEffect = await waitForPersistedEffect(admin, database, unknownJobResponse.data.id, ['sending']);
    const heldBeforeCrash = budgetCounters(admin, database, unknownProject.id);
    assert(sendingEffect.reservationStatus === 'held' && sendingEffect.reservedUnits > 0 &&
      heldBeforeCrash.reservedUnits === sendingEffect.reservedUnits && heldBeforeCrash.spentUnits === 0,
      'sending intent did not retain a nonzero held budget reservation');
    await stopChild(workerProcess, 'SIGKILL');
    workerProcess = null;
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 2500));
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 2, dockerContext });
    const unknownJob = await waitForPersistedJob(admin, database, unknownJobResponse.data.id, ['unknown'], 15_000);
    const unknownEffect = await waitForPersistedEffect(admin, database, unknownJobResponse.data.id, ['unknown']);
    const heldUnknown = budgetCounters(admin, database, unknownProject.id);
    const unknownLifecycle = assertJobLifecycleEvents(
      lifecycleEventsForJob(admin, database, unknownProject.id, unknownJobResponse.data.id), 'job.unknown', 'interrupted send');
    assert(unknownJob.status === 'unknown' && unknownEffect.reservationStatus === 'held' &&
      heldUnknown.reservedUnits === unknownEffect.reservedUnits && heldUnknown.spentUnits === 0 &&
      countForJob(admin, database, 'usage_ledger', unknownJobResponse.data.id) === 0 &&
      (await providerSnapshot(provider)).inferenceRequests === beforeUnknownCall + 1,
      'interrupted sending effect was automatically retransmitted or released its unknown reservation');
    const evidenceId = `synthetic-evidence-${randomBytes(8).toString('hex')}`;
    const reconciliation = await reconcileEffect(testApiOrigin, unknownProject.id,
      unknownEffect.effectId, actorB, evidenceId, { outcome: 'not_processed' }, 'e2e-reconcile-unknown-send');
    assert(reconciliation.response.status === 202 && reconciliation.data.id,
      'budget-only second operator could not queue unknown-effect reconciliation');
    const reconciliationReplay = await reconcileEffect(testApiOrigin, unknownProject.id,
      unknownEffect.effectId, actorB, evidenceId, { outcome: 'not_processed' }, 'e2e-reconcile-unknown-send');
    assert(reconciliationReplay.response.status === 202 && reconciliationReplay.data.id === reconciliation.data.id,
      'identical reconciliation command did not return one stable job');
    const reconciliationConflict = await reconcileEffect(testApiOrigin, unknownProject.id,
      unknownEffect.effectId, actorB, `${evidenceId}-different`, { outcome: 'not_processed' }, 'e2e-reconcile-unknown-send');
    assert(reconciliationConflict.response.status === 409 &&
      !reconciliation.text.includes(evidenceId) && !reconciliationReplay.text.includes(evidenceId),
      'reconciliation idempotency conflict or safe-summary response violated its contract');
    const ownReconciliationJob = await apiRequest(testApiOrigin,
      `/v1/projects/${unknownProject.id}/jobs/${reconciliation.data.id}`, actorB);
    assert(ownReconciliationJob.response.status === 200,
      'budget-only operator could not read its own reconciliation job without project Read');
    assertOpenApiComponent('JobView', ownReconciliationJob.data);
    const otherOperatorsJob = await apiRequest(testApiOrigin,
      `/v1/projects/${unknownProject.id}/jobs/${unknownJobResponse.data.id}`, actorB);
    assert([403, 404].includes(otherOperatorsJob.response.status),
      'budget-only operator could read the original unknown job owned by another actor');
    const reconciliationJob = await waitForJob(testApiOrigin, unknownProject.id,
      reconciliation.data.id, actorB, ['succeeded', 'failed', 'stale'], 20_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    const reconciledOriginal = persistedJob(admin, database, unknownJobResponse.data.id);
    const reconciledEffect = persistedEffect(admin, database, unknownJobResponse.data.id);
    const reconciledBudget = budgetCounters(admin, database, unknownProject.id);
    const reconciledLedgerCount = countForJob(admin, database, 'usage_ledger', unknownJobResponse.data.id);
    assert(reconciliationJob.status === 'succeeded' && reconciledOriginal.status === 'failed' &&
      reconciledEffect.status === 'failed' && reconciledEffect.reservationStatus === 'released' &&
      reconciledBudget.reservedUnits === 0 && reconciledBudget.spentUnits === 0 && reconciledLedgerCount === 0 &&
      (await providerSnapshot(provider)).inferenceRequests === beforeUnknownCall + 1,
      'not_processed reconciliation did not atomically release the held budget without another external call');
    const reconciledEvents = assertJobLifecycleEvents(
      lifecycleEventsForJob(admin, database, unknownProject.id, unknownJobResponse.data.id), 'job.reconciled', 'reconciled unknown send');
    report.effect_recovery = {
      settled_effect_job_status: knownJob.status,
      settled_effect_provider_calls: settledEffectCalls,
      settled_effect_ledger_before_and_after_restart: [settlementLedgerBeforeCrash, settledLedgerAfterRecovery],
      settled_effect_lifecycle_events: knownLifecycle,
      interrupted_job_status: unknownJob.status,
      interrupted_effect_status: unknownEffect.status,
      interrupted_reservation_status: unknownEffect.reservationStatus,
      reconciliation_job_status: reconciliationJob.status,
      reconciled_original_job_status: reconciledOriginal.status,
      reconciled_effect_status: reconciledEffect.status,
      reconciled_reservation_status: reconciledEffect.reservationStatus,
      reconciliation_same_key_stable: reconciliation.data.id === reconciliationReplay.data.id,
      reconciliation_changed_fingerprint_status: reconciliationConflict.response.status,
      budget_only_operator_can_read_own_reconciliation_job: ownReconciliationJob.response.status === 200,
      budget_only_operator_cannot_read_original_job: [403, 404].includes(otherOperatorsJob.response.status),
      unknown_send_provider_calls: (await providerSnapshot(provider)).inferenceRequests - beforeUnknownCall,
      unknown_lifecycle_events: unknownLifecycle,
      reconciliation_lifecycle_events: reconciledEvents,
      budget_after_reconciliation: { reserved_units: reconciledBudget.reservedUnits, spent_units: reconciledBudget.spentUnits },
      usage_ledger_rows_after_not_processed: reconciledLedgerCount,
    };
    // A separate uncertain effect exercises Processed evidence through the real worker.
    const beforeProcessedCall = (await providerSnapshot(provider)).inferenceRequests;
    await configureProviderScenario(provider, { mode: 'malformed' });
    const processedTarget = await enqueueJob(testApiOrigin, unknownProject.id, refreshedSession,
      { kind: 'model_call', request: syntheticModelRequest('Synthetic processed reconciliation.') },
      0, 'e2e-processed-target', { max_attempts: 1, ttl_seconds: 60 });
    assert(processedTarget.response.status === 202, 'processed target was not admitted');
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    await waitForPersistedJob(admin, database, processedTarget.data.id, ['unknown']);
    const processedEffect = persistedEffect(admin, database, processedTarget.data.id);
    const intent = JSON.parse(runPsql(admin, database,
      `SELECT intent FROM effects WHERE id='${processedEffect.effectId}'::uuid;`));
    const heldBeforeProof = budgetCounters(admin, database, unknownProject.id);
    const proofResponse = {
      destination_id: intent.registration.destination_id, provider: intent.registration.provider,
      model: intent.registration.model, model_version: intent.registration.model_version,
      output: { schema_id: intent.registration.output_schema_id,
        schema_version: intent.registration.output_schema_version, data: 42 },
      usage: { input_tokens: 17, output_tokens: 11, cached_input_tokens: 0 },
      pricing: intent.registration.pricing,
    };
    const invalidProof = await reconcileEffect(testApiOrigin, unknownProject.id, processedEffect.effectId,
      actorB, 'synthetic-invalid-schema-proof', { outcome: 'processed', response: proofResponse }, 'e2e-invalid-schema-proof');
    assert(invalidProof.response.status === 202, 'invalid schema evidence fixture was not admitted for worker validation');
    const rejectedProof = await waitForPersistedJob(admin, database, invalidProof.data.id, ['failed', 'succeeded']);
    const heldAfterProof = budgetCounters(admin, database, unknownProject.id);
    assert(rejectedProof.status === 'failed' &&
      JSON.stringify(heldAfterProof) === JSON.stringify(heldBeforeProof) &&
      persistedEffect(admin, database, processedTarget.data.id).status === 'unknown' &&
      countForJob(admin, database, 'usage_ledger', processedTarget.data.id) === 0,
      'invalid schema evidence changed accounting or completed the uncertain effect');
    proofResponse.output.data = { summary: 'Synthetic reconciled answer', items: [] };
    const validProof = await reconcileEffect(testApiOrigin, unknownProject.id, processedEffect.effectId,
      actorB, 'synthetic-valid-schema-proof', { outcome: 'processed', response: proofResponse }, 'e2e-valid-schema-proof');
    assert(validProof.response.status === 202, 'valid processed proof was not admitted');
    const acceptedProof = await waitForPersistedJob(admin, database, validProof.data.id, ['failed', 'succeeded']);
    const acceptedTarget = persistedJob(admin, database, processedTarget.data.id);
    const acceptedEffect = persistedEffect(admin, database, processedTarget.data.id);
    const acceptedBudget = budgetCounters(admin, database, unknownProject.id);
    await stopChild(workerProcess, 'SIGTERM'); workerProcess = null;
    const processedCalls = (await providerSnapshot(provider)).inferenceRequests - beforeProcessedCall;
    assert(acceptedProof.status === 'succeeded' && acceptedTarget.status === 'succeeded' &&
      acceptedEffect.status === 'succeeded' && acceptedEffect.reservationStatus === 'settled' &&
      acceptedBudget.reservedUnits === 0 && acceptedBudget.spentUnits === 28 &&
      countForJob(admin, database, 'usage_ledger', processedTarget.data.id) === 1 && processedCalls === 1,
      'valid processed proof did not settle once and integrate for its original actor');
    report.processed_reconciliation = { invalid_schema_job_status: rejectedProof.status,
      invalid_schema_preserved_hold_and_ledger: true, valid_job_status: acceptedProof.status,
      target_status: acceptedTarget.status, ledger_rows: 1, provider_calls: processedCalls,
      budget: acceptedBudget };
    const publicEffects = await apiRequest(testApiOrigin,
      `/v1/projects/${unknownProject.id}/effects`, refreshedSession);
    const publicEffect = await apiRequest(testApiOrigin,
      `/v1/projects/${unknownProject.id}/effects/${processedEffect.effectId}`, refreshedSession);
    assert(publicEffects.response.status === 200 && publicEffect.response.status === 200,
      'public effect projection could not be read');
    assertOpenApiComponent('EffectRecordView', publicEffect.data);
    for (const effect of [...publicEffects.data.items, publicEffect.data]) {
      assert(!Object.hasOwn(effect, 'fingerprint') && !Object.hasOwn(effect.intent, 'fingerprint'),
        'public effect DTO exposed a deterministic private-input fingerprint');
    }
    assert(Array.isArray(intent.fingerprint) && intent.fingerprint.length === 32,
      'durable effect intent lost its private idempotency fingerprint');
    report.effect_privacy = { list_and_detail_omit_input_fingerprint: true,
      durable_intent_retains_fingerprint: true };
    report.model_key_delivery = { api_process_has_model_key: false, worker_process_has_ephemeral_model_key: true };
    report.criteria['P1-10'] = { status: 'passed', checks: ['known provider settlement survived SIGKILL before terminal job write and did not resend', 'interrupted sending became unknown and retained held reservation', 'different budget-only operator reconciled idempotently with zero additional provider requests', 'Processed proof rejects invalid schema without accounting changes and integrates valid schema once'] };

    currentCriterion = 'P1-11';
    const gatewayProject = await createProject(testApiOrigin, refreshedSession,
      orgA.id, 'Synthetic gateway-boundary checks');
    const beforeInvalidInputs = (await providerSnapshot(provider)).inferenceRequests;
    const invalidGatewayRequests = await Promise.all([
      enqueueJob(testApiOrigin, gatewayProject.id, refreshedSession,
        { kind: 'model_call', request: { ...syntheticModelRequest(), destination_id: 'unregistered-destination' } },
        0, 'e2e-forbidden-destination'),
      enqueueJob(testApiOrigin, gatewayProject.id, refreshedSession,
        { kind: 'model_call', request: { ...syntheticModelRequest(), input: { ...syntheticModelRequest().input, categories: ['secret'] } } },
        0, 'e2e-forbidden-secret-category'),
      enqueueJob(testApiOrigin, gatewayProject.id, refreshedSession,
        { kind: 'model_call', request: { ...syntheticModelRequest(), secret_ref: 'synthetic-forbidden-secret-reference' } },
        0, 'e2e-client-secret-reference'),
    ]);
    assert(invalidGatewayRequests.every((item, index) => index === 0
      ? item.response.status === 503 && item.data?.error?.code === 'service_unavailable'
      : item.response.status >= 400 && item.response.status < 500) &&
      runPsql(admin, database, `SELECT count(*) FROM jobs WHERE project_id='${gatewayProject.id}'::uuid;`) === '0',
      'unregistered destination, secret category, or client secret reference was accepted into the durable queue');
    report.gateway_admission_refusal_statuses = invalidGatewayRequests.map((item) => item.response.status);
    assert((await providerSnapshot(provider)).inferenceRequests === beforeInvalidInputs,
      'gateway admission rejection caused an external provider request');

    const malformedProject = gatewayProject;
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    const gatewayResults = [];
    for (const scenario of [
      { name: 'malformed', mode: 'malformed' },
      { name: 'oversized', mode: 'oversized', responseBytes: 64 * 1024 },
      { name: 'error-with-canary', mode: 'status', statusCode: 503, failuresRemaining: 1, canary: true },
    ]) {
      const beforeScenarioCalls = (await providerSnapshot(provider)).inferenceRequests;
      const { name: scenarioName, canary: needsCanary, ...inferenceScenario } = scenario;
      await configureProviderScenario(provider, inferenceScenario);
      const canary = needsCanary ? `KYRO_E2E_SECRET_SENTINEL_${randomBytes(12).toString('hex')}` : null;
      if (canary) {
        auditCanary = canary;
        report.secret_sentinel_included_in_test_input = true;
      }
      const submitted = await enqueueJob(testApiOrigin, malformedProject.id, refreshedSession,
        { kind: 'model_call', request: syntheticModelRequest(canary ?? `Synthetic ${scenarioName} response probe.`) },
        0, `e2e-gateway-${scenario.name}`, { max_attempts: 1, ttl_seconds: 60 });
      assert(submitted.response.status === 202, `${scenario.name} model probe was not admitted`);
      const job = await waitForJob(testApiOrigin, malformedProject.id,
        submitted.data.id, refreshedSession, ['failed', 'unknown', 'stale'], 20_000);
      const callsAfterScenario = (await providerSnapshot(provider)).inferenceRequests;
      assert(callsAfterScenario === beforeScenarioCalls + 1,
        `${scenario.name} provider response caused a retry or bypassed the one-attempt limit`);
      if (canary) {
        const publicJob = await apiRequest(testApiOrigin,
          `/v1/projects/${malformedProject.id}/jobs/${submitted.data.id}`, refreshedSession);
        const sqlCanaryCounts = runPsql(admin, database,
          `SELECT (SELECT count(*) FROM public.events WHERE project_id='${malformedProject.id}'::uuid AND position('${canary}' in payload::text)>0) || '|' || ` +
          `(SELECT count(*) FROM public.outbox_events WHERE project_id='${malformedProject.id}'::uuid AND position('${canary}' in payload::text)>0);`);
        const [eventMatches, outboxMatches] = sqlCanaryCounts.split('|').map(Number);
        const processLogs = `${apiProcess?.logs?.() ?? ''}\n${workerProcess?.logs?.() ?? ''}`;
        const sentinelInResponses = observedResponseBodies.some((value) => value.includes(canary)) ||
          submitted.text.includes(canary) || publicJob.text.includes(canary);
        const sentinelInLogs = processLogs.includes(canary);
        sentinelLogCheck = !sentinelInLogs;
        assert(!sentinelInResponses && !sentinelInLogs && eventMatches === 0 && outboxMatches === 0,
          'synthetic private-input canary appeared in a public response, service log, event, or outbox payload');
        report.synthetic_secret_sentinel_absent_from_http_logs_and_events = true;
        auditCanary = null;
      }
      gatewayResults.push({ scenario: scenario.name, terminal_status: job.status, provider_calls: callsAfterScenario - beforeScenarioCalls });
    }
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    const gatewayCallCount = (await providerSnapshot(provider)).inferenceRequests - beforeInvalidInputs;
    assert(gatewayCallCount === 3, 'P1-11 malformed/oversized/error probes did not record exactly three provider calls');
    report.gateway_bounds = {
      http_body_limit_bytes: 48 * 1024,
      response_policy_limit_bytes: 48_000,
      invalid_destination_category_and_secret_reference_refused: invalidGatewayRequests.length,
      invalid_request_provider_calls: 0,
      bounded_output_scenarios: gatewayResults,
      secret_sentinel_absent_from_http_logs_events_and_outbox: report.synthetic_secret_sentinel_absent_from_http_logs_and_events === true,
    };
    report.criteria['P1-11'] = { status: 'passed', checks: ['unregistered destination, secret category, and client secret reference refused before dispatch', 'malformed and oversized structured responses terminated without retry', 'error path kept a synthetic private-input canary out of public responses, service logs, events, and outbox'] };

    currentCriterion = 'P1-14';
    const metricsApiOrigin = testApiOrigin;
    const metricsBefore = await readPrometheusMetrics(metricsApiOrigin);
    const metricsBeforeKeys = [...metricsBefore.samples.keys()].sort();
    const metricsProbeCount = 5;
    const metricsJobs = [];
    const metricsRandomPaths = [];
    for (let index = 0; index < metricsProbeCount; index += 1) {
      const suffix = randomBytes(8).toString('hex');
      const metricsProject = await createProject(metricsApiOrigin, refreshedSession, orgA.id,
        `Synthetic metric cardinality ${runToken}-${index}-${suffix}`);
      const snapshot = await apiRequest(metricsApiOrigin, `/v1/projects/${metricsProject.id}`, refreshedSession);
      assert(snapshot.response.status === 200, 'metrics probe project snapshot was not readable');
      const queued = await enqueueJob(metricsApiOrigin, metricsProject.id, refreshedSession,
        { kind: 'apply_changes', changes: { operations: [
          { op: 'set_preference', key: `metrics-${suffix}`, value: index },
        ] } }, 0, `e2e-metrics-${runToken}-${index}-${suffix}`);
      assert(queued.response.status === 202, 'metrics probe job was not admitted');
      const detail = await apiRequest(metricsApiOrigin,
        `/v1/projects/${metricsProject.id}/jobs/${queued.data.id}`, refreshedSession);
      assert(detail.response.status === 200 && detail.data.id === queued.data.id,
        'metrics probe job detail was not readable');
      metricsJobs.push({ projectId: metricsProject.id, jobId: queued.data.id });
      const randomPath = `/__e2e_metrics_probe/${randomBytes(12).toString('hex')}/${randomBytes(12).toString('hex')}`;
      const unknownRoute = await fetch(`${metricsApiOrigin}${randomPath}`, { signal: AbortSignal.timeout(3000) });
      assert([404, 405].includes(unknownRoute.status), 'random metrics path unexpectedly matched an API route');
      metricsRandomPaths.push({ status: unknownRoute.status });
    }
    const otherMethodPath = `/__e2e_metrics_probe/${randomBytes(12).toString('hex')}`;
    const otherMethod = await fetch(`${metricsApiOrigin}${otherMethodPath}`, {
      method: 'PROPFIND', signal: AbortSignal.timeout(3000),
    });
    assert(otherMethod.status >= 400 && otherMethod.status < 500,
      'unlisted HTTP method probe did not produce a bounded client-error class');

    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    const metricsJobResults = [];
    for (const item of metricsJobs) {
      const terminal = await waitForJob(metricsApiOrigin, item.projectId, item.jobId, refreshedSession,
        ['succeeded', 'failed', 'stale', 'cancelled'], 20_000);
      assert(terminal.status === 'succeeded', 'metrics probe local job did not reach successful terminal state');
      metricsJobResults.push(terminal.status);
    }
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    const metricsAfter = await readPrometheusMetrics(metricsApiOrigin);
    const metricsAfterKeys = [...metricsAfter.samples.keys()].sort();
    const allowedMetricNames = new Set([
      'kyro_http_requests_total',
      'kyro_http_response_headers_seconds_bucket',
      'kyro_http_response_headers_seconds_sum',
      'kyro_http_response_headers_seconds_count',
    ]);
    for (const sample of metricsAfter.samples.values()) {
      assert(allowedMetricNames.has(sample.name), 'runtime metrics emitted an unreviewed metric family');
      const labelNames = Object.keys(sample.labels).sort();
      const expectedNames = sample.name === 'kyro_http_requests_total'
        ? ['method', 'status_class']
        : sample.name === 'kyro_http_response_headers_seconds_bucket' ? ['le'] : [];
      assert(isDeepStrictEqual(labelNames, expectedNames),
        'runtime metric labels included a path, project, job, actor, or request identifier');
    }
    assert(isDeepStrictEqual(metricsBeforeKeys, metricsAfterKeys),
      'runtime metric series cardinality changed after randomized projects, jobs, and paths');
    const getSuccessDelta = metricValue(metricsAfter.samples, 'kyro_http_requests_total', {
      method: 'GET', status_class: '2xx',
    }) - metricValue(metricsBefore.samples, 'kyro_http_requests_total', {
      method: 'GET', status_class: '2xx',
    });
    const otherClientErrorDelta = metricValue(metricsAfter.samples, 'kyro_http_requests_total', {
      method: 'OTHER', status_class: '4xx',
    }) - metricValue(metricsBefore.samples, 'kyro_http_requests_total', {
      method: 'OTHER', status_class: '4xx',
    });
    assert(getSuccessDelta >= metricsProbeCount * 3,
      'runtime request counter did not record project/job HTTP probes');
    assert(otherClientErrorDelta === 1,
      'runtime request counter did not classify the unlisted method as bounded OTHER/4xx');
    const responseCount = metricValue(metricsAfter.samples, 'kyro_http_response_headers_seconds_count');
    assert(responseCount > 0, 'runtime latency histogram did not record HTTP responses');
    report.runtime_metrics = {
      endpoint_status: 200,
      request_counter: 'kyro_http_requests_total',
      latency_histogram: 'kyro_http_response_headers_seconds',
      project_probes: metricsProbeCount,
      terminal_local_jobs: metricsJobResults.length,
      randomized_unmatched_paths: metricsRandomPaths.length,
      randomized_unmatched_path_status_classes: [...new Set(metricsRandomPaths.map((item) => Math.floor(item.status / 100)))].sort(),
      unlisted_method_status_class: Math.floor(otherMethod.status / 100),
      unlisted_method_counter_delta: otherClientErrorDelta,
      observed_successful_get_counter_delta: getSuccessDelta,
      response_histogram_count: responseCount,
      series_cardinality_before: metricsBefore.samples.size,
      series_cardinality_after: metricsAfter.samples.size,
      metric_families: [...allowedMetricNames].sort(),
      label_names: { request_counter: ['method', 'status_class'], latency_histogram_bucket: ['le'] },
      exposition_sample: metricsAfter.exposition.trim().split(/\r?\n/),
    };
    report.criteria['P1-14'] = {
      status: 'partial',
      checks: ['runtime Prometheus endpoint emitted request counter and latency histogram',
        'series and bounded labels stayed constant after randomized project/job/path probes',
        'unlisted method was counted under OTHER/4xx'],
      reason: 'documentation, dependency/OpenAPI review, and independent review remain separate checks',
    };

    currentCriterion = 'P1-13';
    const restoreProject = await createProject(testApiOrigin, refreshedSession,
      orgA.id, 'Synthetic prepared-effect restore fixture');
    const restoreInitialSnapshot = await apiRequest(testApiOrigin,
      `/v1/projects/${restoreProject.id}`, refreshedSession);
    assert(restoreInitialSnapshot.response.status === 200 && restoreInitialSnapshot.data.project.current_revision === 0,
      'restore fixture did not start at revision zero');
    const pausedModelJob = await enqueueJob(testApiOrigin, restoreProject.id, refreshedSession,
      { kind: 'model_call', request: syntheticModelRequest('Synthetic model held by the durable send pause.') },
      0, 'e2e-restore-paused-model', { max_attempts: 3, ttl_seconds: 60 });
    assert(pausedModelJob.response.status === 202, 'model pause fixture was not admitted before the durable pause');
    const providerCallsBeforePause = (await providerSnapshot(provider)).inferenceRequests;
    runPsql(admin, database,
      'UPDATE public.runtime_control SET external_sends_enabled=FALSE, updated_at=clock_timestamp() WHERE id=1;');
    const localDuringPause = await enqueueJob(testApiOrigin, restoreProject.id, refreshedSession,
      { kind: 'apply_changes', changes: { operations: [
        { op: 'set_preference', key: 'restore.pause.local', value: true },
      ] } }, 0, 'e2e-restore-local-during-model-pause');
    assert(localDuringPause.response.status === 202, 'same-project local command was not admitted while outbound sends were paused');
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    const localDuringPauseResult = await waitForJob(testApiOrigin, restoreProject.id,
      localDuringPause.data.id, refreshedSession, ['succeeded', 'failed', 'stale', 'cancelled'], 20_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    const pausedModelPersisted = persistedJob(admin, database, pausedModelJob.data.id);
    assert(localDuringPauseResult.status === 'succeeded' && pausedModelPersisted.status === 'pending' &&
      pausedModelPersisted.attempts === 0 && pausedModelPersisted.generation === 0 &&
      (await providerSnapshot(provider)).inferenceRequests === providerCallsBeforePause,
      'same-project ApplyChanges failed while the ModelCall pause consumed an attempt or reached the provider');
    const cancelledPausedModel = await apiRequest(testApiOrigin,
      `/v1/projects/${restoreProject.id}/jobs/${pausedModelJob.data.id}`, refreshedSession, { method: 'DELETE' });
    if (cancelledPausedModel.response.status === 202) assertOpenApiComponent('JobView', cancelledPausedModel.data);
    assert(cancelledPausedModel.response.status === 202 && cancelledPausedModel.data.status === 'pending' && cancelledPausedModel.data.cancel_requested === true,
      'paused model cancellation request was not persisted');
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 10, dockerContext });
    const pausedCancellationTerminal = await waitForJob(testApiOrigin, restoreProject.id,
      pausedModelJob.data.id, refreshedSession, ['cancelled'], 20_000);
    await stopChild(workerProcess, 'SIGTERM');
    workerProcess = null;
    assert((await providerSnapshot(provider)).inferenceRequests === providerCallsBeforePause,
      'cancelled paused model job reached the provider');
    runPsql(admin, database,
      'UPDATE public.runtime_control SET external_sends_enabled=TRUE, updated_at=clock_timestamp() WHERE id=1;');
    const restoreCurrentSnapshot = await apiRequest(testApiOrigin,
      `/v1/projects/${restoreProject.id}`, refreshedSession);
    assert(restoreCurrentSnapshot.response.status === 200 && restoreCurrentSnapshot.data.project.current_revision === 1,
      'same-project local command did not advance the restore fixture revision');
    report.restore_pause = {
      same_project: true,
      local_command_terminal_status: localDuringPauseResult.status,
      paused_model_status_before_cancel: pausedModelPersisted.status,
      paused_model_attempts: pausedModelPersisted.attempts,
      paused_model_generation: pausedModelPersisted.generation,
      provider_calls_while_paused: (await providerSnapshot(provider)).inferenceRequests - providerCallsBeforePause,
      paused_model_cancel_status: pausedCancellationTerminal.status,
    };
    const restoreSendingProject = await createProject(testApiOrigin, refreshedSession,
      orgA.id, 'Synthetic interrupted-sending restore fixture');
    await configureProviderScenario(provider, { mode: 'hold' });
    const beforeRestoreSend = (await providerSnapshot(provider)).inferenceRequests;
    const interruptedRestoreJob = await enqueueJob(testApiOrigin, restoreSendingProject.id, refreshedSession,
      { kind: 'model_call', request: syntheticModelRequest('Synthetic restore fixture with prepared outbound intent.') },
      0, 'e2e-sending-effect-restore-fixture',
      { max_attempts: 3, ttl_seconds: 60 });
    assert(interruptedRestoreJob.response.status === 202, 'nonempty sending-effect restore job was not admitted');
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 30, dockerContext });
    await waitForProviderCount(provider, beforeRestoreSend + 1);
    const sendingBeforeRestore = await waitForPersistedEffect(admin, database, interruptedRestoreJob.data.id, ['sending']);
    const sendingJobBeforeRestore = persistedJob(admin, database, interruptedRestoreJob.data.id);
    const sendingBudgetBeforeRestore = budgetCounters(admin, database, restoreSendingProject.id);
    assert(sendingJobBeforeRestore.status === 'running' && sendingBeforeRestore.reservationStatus === 'held' &&
      sendingBeforeRestore.reservedUnits > 0 && sendingBudgetBeforeRestore.reservedUnits === sendingBeforeRestore.reservedUnits &&
      sendingBudgetBeforeRestore.spentUnits === 0,
    'restore fixture did not capture a real sending effect with a nonzero held reservation');
    await stopChild(workerProcess, 'SIGKILL');
    workerProcess = null;
    await releaseProviderInference(provider);
    await configureProviderScenario(provider, { mode: 'success' });
    const sendingAfterKill = persistedEffect(admin, database, interruptedRestoreJob.data.id);
    assert(sendingAfterKill?.status === 'sending' && persistedJob(admin, database, interruptedRestoreJob.data.id).status === 'running',
      'SIGKILL did not leave the outbound effect durably sending before restoration');

    const restoreJobResponse = await enqueueJob(testApiOrigin, restoreProject.id, refreshedSession,
      { kind: 'model_call', request: syntheticModelRequest('Synthetic prepared outbound intent held for restore.') },
      restoreCurrentSnapshot.data.project.current_revision, 'e2e-prepared-effect-restore-fixture',
      { max_attempts: 3, ttl_seconds: 60 });
    assert(restoreJobResponse.response.status === 202, 'nonempty prepared-effect restore job was not admitted');
    const preparedIntentTrigger = {
      functionName: `e2e_block_restore_send_${runToken}`,
      triggerName: `e2e_block_restore_send_${runToken}`,
      table: 'effects',
      predicate: `NEW.status='sending' AND NEW.job_id='${restoreJobResponse.data.id}'::uuid`,
    };
    installBlockTrigger(admin, database, preparedIntentTrigger);
    workerProcess = startWorkerProcess(workerUrl, key, runToken, { pollMs: 25, leaseSeconds: 30, dockerContext });
    await waitForWorkerSleep(admin, database, workerApplication);
    const preparedBeforeRestore = await waitForPersistedEffect(admin, database, restoreJobResponse.data.id, ['prepared']);
    const preparedJobBeforeRestore = persistedJob(admin, database, restoreJobResponse.data.id);
    const preparedBudgetBeforeRestore = budgetCounters(admin, database, restoreProject.id);
    assert(preparedJobBeforeRestore.status === 'running' && preparedBeforeRestore.reservationStatus === 'held' &&
      preparedBeforeRestore.reservedUnits > 0 && preparedBudgetBeforeRestore.reservedUnits === preparedBeforeRestore.reservedUnits &&
      preparedBudgetBeforeRestore.spentUnits === 0 &&
      (await providerSnapshot(provider)).inferenceRequests === beforeRestoreSend + 1,
      'restore fixture did not stop between durable preparation and external send');
    await stopChild(workerProcess, 'SIGKILL');
    workerProcess = null;
    removeBlockTrigger(admin, database, preparedIntentTrigger);
    const preparedAtBackup = persistedJob(admin, database, restoreJobResponse.data.id);
    const preparedEffectAtBackup = persistedEffect(admin, database, restoreJobResponse.data.id);
    const sendingAtBackup = persistedJob(admin, database, interruptedRestoreJob.data.id);
    const sendingEffectAtBackup = persistedEffect(admin, database, interruptedRestoreJob.data.id);
    assert(preparedAtBackup.status === 'running' && preparedEffectAtBackup?.status === 'prepared' &&
      preparedEffectAtBackup.reservationStatus === 'held' && sendingAtBackup.status === 'running' &&
      sendingEffectAtBackup?.status === 'sending' && sendingEffectAtBackup.reservationStatus === 'held',
    'source database did not contain both required recovery states immediately before backup');
    report.restore_source_preconditions = {
      projects_are_distinct: restoreProject.id !== restoreSendingProject.id,
      prepared_job_status: preparedAtBackup.status,
      prepared_effect_status: preparedEffectAtBackup.status,
      prepared_reservation_status: preparedEffectAtBackup.reservationStatus,
      sending_job_status: sendingAtBackup.status,
      sending_effect_status: sendingEffectAtBackup.status,
      sending_reservation_status: sendingEffectAtBackup.reservationStatus,
      sending_provider_call_count: beforeRestoreSend + 1,
    };
    await stopChild(apiProcess, 'SIGTERM');
    apiProcess = null;
    restoreDatabase = `kyro_restore_${runToken}`;
    report.restore = await backupAndRestoreFixture(
      admin, database, restoreDatabase, restoreJobResponse.data.id, interruptedRestoreJob.data.id,
      options, dockerContext);
    report.restore_database_created = true;
    const providerCallsBeforeRestoreRuntime = (await providerSnapshot(provider)).inferenceRequests;
    assert(providerCallsBeforeRestoreRuntime === beforeRestoreSend + 1,
      'database backup/restore unexpectedly resumed an external provider send');
    report.restore_runtime = await verifyRestoredRuntime({
      admin,
      adminUrl,
      restoreDatabase,
      preparedJobId: restoreJobResponse.data.id,
      sendingJobId: interruptedRestoreJob.data.id,
      projectId: restoreSendingProject.id,
      preparedProjectId: restoreProject.id,
      executeProjectId: projectA.id,
      organizationId: orgA.id,
      actorId: actorA.actorId,
      preRestoreCookieHeader: refreshedSession.cookieHeader,
      provider,
      controlToken,
      apiKey: key,
      runToken,
      options,
      dockerContext,
    });
    report.restore_runtime.inference_calls_before_worker = providerCallsBeforeRestoreRuntime;
    assert(report.restore_runtime.provider_call_delta_after_worker_start === 0,
      'restored worker emitted another provider request after restart');
    report.restore.worker_started_after_restore_for_verification = true;
    report.restore.worker_restart_after_restore_tested_separately = true;
    report.restore.integrity_fingerprinted_tables = 16;
    report.criteria['P1-13'] = { status: 'passed', checks: [
      'nonempty real PostgreSQL archive restored into a new isolated database',
      'canonical fingerprints matched for actors, organizations, memberships, projects, grants including limits, revisions, decisions, change commands, jobs, effects, budget state, ledger, events, outbox, and sessions',
      'prepared effect and SIGKILL-interrupted sending effect both retained held reservations; sending recovered to unknown without replay',
      'runtime control remained disabled after worker restart and old pre-restore cookie was rejected',
      'fresh synthetic OIDC login resolved persisted actor/owner membership and admitted ApplyChanges through the existing Execute grant',
      'restored worker completed that command using Write; the expected preference and next revision were persisted',
      'same-project local ReconcileEffect completed while restored ModelCalls had unchanged attempts and zero new provider calls',
    ] };
    report.criteria['P1-14'] = { status: 'partial', reason: 'report and repeatable runner are present; independent review is not executed by this harness' };
    report.status = 'runtime_verified_review_pending';
    report.failure = null;
    currentCriterion = null;
  } catch (error) {
    report.status = 'failed_or_blocked';
    report.failure = error instanceof Error ? error.message : 'unknown test runner failure';
    if (currentCriterion && ['not_run', 'partial'].includes(report.criteria[currentCriterion].status)) {
      report.criteria[currentCriterion] = {
        ...report.criteria[currentCriterion],
        status: 'failed',
        reason: report.failure,
      };
    }
  } finally {
    report.source_unchanged = captureSourceState().sha256 === report.source.sha256;
    if (!report.source_unchanged) {
      report.status = 'failed_or_blocked';
      report.source_change_failure = 'build/test inputs changed during acceptance; rerun against a stable fingerprint';
      report.failure ??= report.source_change_failure;
    }
    report.synthetic_secret_sentinel_absent_from_process_logs = report.secret_sentinel_included_in_test_input
      ? sentinelLogCheck === true
      : null;
    if (report.secret_sentinel_included_in_test_input && sentinelLogCheck !== true && report.status !== 'failed_or_blocked') {
      report.status = 'failed_or_blocked';
      report.failure = 'synthetic canary was found in a captured service log';
    }
    if (provider) {
      try {
        report.measured_provider_requests = (await providerSnapshot(provider)).inferenceRequests;
      } catch {
        report.measured_provider_requests = 'unavailable after provider failure';
      }
    }
    for (const worker of additionalWorkerProcesses) await stopChild(worker, 'SIGTERM').catch(() => {});
    await stopChild(workerProcess, 'SIGTERM').catch(() => {});
    await stopChild(apiProcess, 'SIGTERM').catch(() => {});
    await provider?.close().catch(() => {});
    if (dockerContext?.appImage) docker(['image', 'rm', dockerContext.appImage], { allowFailure: true });
    const p1RuntimeScenariosPassed = Array.from({ length: 13 }, (_, index) =>
      report.criteria[`P1-${String(index + 1).padStart(2, '0')}`]?.status === 'passed').every(Boolean);
    if (database && p1RuntimeScenariosPassed && report.status === 'runtime_verified_review_pending' && !options.keepOnFailure) {
      if (restoreDatabase) {
        try {
          runPsql(admin, admin.database, `DROP DATABASE ${safeIdentifier(restoreDatabase)};`);
          report.restore_database_dropped_after_success = true;
        } catch {
          report.restore_database_dropped_after_success = false;
        }
      }
      try {
        runPsql(admin, admin.database, `DROP DATABASE ${safeIdentifier(database)};`);
        report.database_dropped_after_success = true;
      } catch {
        report.database_dropped_after_success = false;
      }
    } else {
      report.database_dropped_after_success = false;
      report.restore_database_dropped_after_success = false;
    }
    report.finished_at = new Date().toISOString();
    try {
      report.evidence_path = relative(repoRoot, writeEvidence(report, runId)).replaceAll('\\', '/');
    } catch {
      report.evidence_path = null;
    }
  }
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  return report.status === 'runtime_verified_review_pending' ? 0 : 1;
}

try {
  const options = parseArgs(process.argv.slice(2));
  testApiPort = options.apiPort;
  testApiOrigin = `http://127.0.0.1:${testApiPort}`;
  testProviderPort = options.providerPort;
  testControlPort = options.controlPort;
  if (testProviderPort !== 9090 && ['run', 'diagnose-p1-06'].includes(options.mode)) {
    syntheticRegistryDirectory = mkdtempSync(join(tmpdir(), 'kyro-synthetic-ports-'));
    syntheticRegistryPath = join(syntheticRegistryDirectory, 'registry.json');
    const registry = JSON.parse(readFileSync(resolve(repoRoot, 'tests/fixtures/models.synthetic.e2e.json'), 'utf8'));
    registry.destinations[0].base_url = `http://127.0.0.1:${testProviderPort}/v1/`;
    registry.destinations[0].pinned_addresses = [`127.0.0.1:${testProviderPort}`];
    writeFileSync(syntheticRegistryPath, JSON.stringify(registry));
  }
  if (options.mode === 'fingerprint') process.stdout.write(`${JSON.stringify(captureSourceState(), null, 2)}\n`);
  else if (options.mode === 'help') process.stdout.write(usage());
  else if (options.mode === 'preflight') process.exitCode = await runPreflight(options);
  else if (options.mode === 'diagnose-p1-06') process.exitCode = await runP106Diagnostic(options);
  else process.exitCode = await runAcceptance(options);
} catch (error) {
  process.stderr.write(`verify-p1: ${error.message}\n`);
  process.exitCode = 1;
} finally {
  if (syntheticRegistryDirectory) {
    rmSync(join(syntheticRegistryDirectory, 'registry.json'));
    rmdirSync(syntheticRegistryDirectory);
  }
}
