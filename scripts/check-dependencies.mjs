import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const TARGET = "x86_64-unknown-linux-gnu";
const WORKSPACE_ROOTS = new Set([
  "kyro-api",
  "kyro-domain",
  "kyro-gateway",
  "kyro-store",
  "kyro-worker",
]);
const MAX_OUTPUT_BYTES = 64 * 1024 * 1024;

function verificationError(code) {
  const error = new Error(code);
  error.code = code;
  return error;
}

function parseJson(text, code) {
  try {
    return JSON.parse(text);
  } catch {
    throw verificationError(code);
  }
}

function parsePackageLine(line) {
  const match = /^([A-Za-z0-9][A-Za-z0-9_.-]*) v([^\s()]+)(?: \([^()\r\n]*\))*$/.exec(line);
  if (!match) throw verificationError("invalid_cargo_tree");
  return { name: match[1], version: match[2] };
}

function packageKey({ name, version }) {
  return `${name}@${version}`;
}

function validateMetadata(metadata) {
  if (
    !metadata ||
    !Array.isArray(metadata.packages) ||
    !Array.isArray(metadata.workspace_members) ||
    !metadata.resolve ||
    !Array.isArray(metadata.resolve.nodes) ||
    metadata.resolve.nodes.length === 0
  ) {
    throw verificationError("missing_metadata_graph");
  }

  const packagesById = new Map();
  const packagesByKey = new Map();
  for (const pkg of metadata.packages) {
    if (
      !pkg ||
      typeof pkg.id !== "string" ||
      typeof pkg.name !== "string" ||
      typeof pkg.version !== "string" ||
      !(pkg.source === null || typeof pkg.source === "string")
    ) {
      throw verificationError("invalid_metadata_package");
    }
    if (packagesById.has(pkg.id)) throw verificationError("duplicate_metadata_package");
    packagesById.set(pkg.id, pkg);
    const key = packageKey(pkg);
    const matches = packagesByKey.get(key) ?? [];
    matches.push(pkg);
    packagesByKey.set(key, matches);
  }

  if (metadata.workspace_members.length !== WORKSPACE_ROOTS.size) {
    throw verificationError("workspace_roots_mismatch");
  }
  const rootNames = new Set();
  const rootPackages = new Map();
  for (const id of metadata.workspace_members) {
    const pkg = packagesById.get(id);
    if (!pkg || pkg.source !== null || !WORKSPACE_ROOTS.has(pkg.name) || rootNames.has(pkg.name)) {
      throw verificationError("workspace_roots_mismatch");
    }
    rootNames.add(pkg.name);
    rootPackages.set(packageKey(pkg), pkg.name);
  }
  if (rootNames.size !== WORKSPACE_ROOTS.size) {
    throw verificationError("workspace_roots_mismatch");
  }

  const nodeIds = new Set();
  for (const node of metadata.resolve.nodes) {
    if (!node || typeof node.id !== "string" || !packagesById.has(node.id) || nodeIds.has(node.id)) {
      throw verificationError("missing_metadata_graph");
    }
    nodeIds.add(node.id);
  }
  for (const id of metadata.workspace_members) {
    if (!nodeIds.has(id)) throw verificationError("missing_metadata_graph");
  }

  return { packagesByKey, rootPackages };
}

function parseActiveTree(treeText, { packagesByKey, rootPackages }) {
  if (typeof treeText !== "string" || treeText.trim().length === 0) {
    throw verificationError("missing_cargo_tree");
  }

  const groups = treeText
    .replace(/\r\n/g, "\n")
    .trim()
    .split(/\n\s*\n/)
    .filter((group) => group.trim().length > 0);
  if (groups.length !== WORKSPACE_ROOTS.size) {
    throw verificationError("workspace_roots_mismatch");
  }

  const observedRoots = new Set();
  const active = new Set();
  for (const group of groups) {
    const lines = group.split("\n");
    const parsed = lines.map((line) => parsePackageLine(line));
    const root = parsed[0];
    const rootKey = packageKey(root);
    const rootName = rootPackages.get(rootKey);
    if (!rootName || observedRoots.has(rootName)) {
      throw verificationError("workspace_roots_mismatch");
    }
    observedRoots.add(rootName);

    for (const pkg of parsed) {
      const key = packageKey(pkg);
      const matches = packagesByKey.get(key);
      if (!matches || matches.length !== 1) {
        throw verificationError("tree_metadata_mismatch");
      }
      active.add(key);
    }
  }
  if (observedRoots.size !== WORKSPACE_ROOTS.size) {
    throw verificationError("workspace_roots_mismatch");
  }
  return { active, roots: [...observedRoots].sort() };
}

function parseAudit(audit, auditExitCode) {
  const vulnerabilities = audit?.vulnerabilities;
  if (
    !vulnerabilities ||
    typeof vulnerabilities.found !== "boolean" ||
    !Number.isInteger(vulnerabilities.count) ||
    vulnerabilities.count < 0 ||
    !Array.isArray(vulnerabilities.list) ||
    vulnerabilities.count !== vulnerabilities.list.length ||
    vulnerabilities.found !== (vulnerabilities.list.length > 0)
  ) {
    throw verificationError("invalid_audit_schema");
  }
  if (
    (auditExitCode === 0 && vulnerabilities.list.length !== 0) ||
    (auditExitCode === 1 && vulnerabilities.list.length === 0) ||
    ![0, 1].includes(auditExitCode)
  ) {
    throw verificationError("audit_command_failed");
  }

  return vulnerabilities.list.map((finding) => {
    const id = finding?.advisory?.id;
    const pkg = finding?.package;
    if (
      typeof id !== "string" ||
      !/^[A-Za-z0-9][A-Za-z0-9._-]{2,}$/.test(id) ||
      !pkg ||
      typeof pkg.name !== "string" ||
      typeof pkg.version !== "string" ||
      typeof pkg.source !== "string"
    ) {
      throw verificationError("invalid_audit_schema");
    }
    return { id, name: pkg.name, version: pkg.version, source: pkg.source };
  });
}

export function classifyDependencyAudit({
  auditText,
  metadataText,
  treeText,
  auditExitCode,
  metadataExitCode = 0,
  treeExitCode = 0,
}) {
  if (metadataExitCode !== 0) throw verificationError("metadata_command_failed");
  if (treeExitCode !== 0) throw verificationError("cargo_tree_failed");

  const audit = parseJson(auditText, "invalid_audit_json");
  const metadata = parseJson(metadataText, "invalid_metadata_json");
  const metadataIndex = validateMetadata(metadata);
  const { active, roots } = parseActiveTree(treeText, metadataIndex);
  const findings = parseAudit(audit, auditExitCode);
  const activeVulnerabilities = [];
  const inactiveLockAdvisories = [];

  for (const finding of findings) {
    const key = packageKey(finding);
    const lockedMatches = metadataIndex.packagesByKey.get(key);
    if (!lockedMatches || lockedMatches.length !== 1 || lockedMatches[0].source !== finding.source) {
      throw verificationError("audit_metadata_mismatch");
    }
    const safeFinding = { id: finding.id, package: finding.name, version: finding.version };
    if (active.has(key)) activeVulnerabilities.push(safeFinding);
    else inactiveLockAdvisories.push(safeFinding);
  }

  const status = activeVulnerabilities.length > 0
    ? "active_vulnerabilities_found"
    : inactiveLockAdvisories.length > 0
      ? "active_graph_clear_inactive_lock_advisories"
      : "active_graph_clear_no_lock_advisories";
  return {
    exitCode: activeVulnerabilities.length > 0 ? 1 : 0,
    summary: {
      schemaVersion: 1,
      status,
      target: TARGET,
      workspaceRoots: roots,
      activePackageCount: active.size,
      auditExitCode,
      lockedAdvisoryCount: findings.length,
      activeVulnerabilities,
      inactiveLockAdvisories,
    },
  };
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: process.cwd(),
    encoding: "utf8",
    maxBuffer: MAX_OUTPUT_BYTES,
    windowsHide: true,
  });
  if (result.error) {
    return { stdout: result.stdout ?? "", status: null, spawnError: result.error.code ?? "spawn_failed" };
  }
  return { stdout: result.stdout ?? "", status: result.status, spawnError: null };
}

function parseEvidenceArgument(argv) {
  if (argv.length !== 2 || argv[0] !== "--evidence" || !argv[1]) {
    throw verificationError("invalid_arguments");
  }
  return argv[1];
}

function emitFailure(error) {
  const code = typeof error?.code === "string" ? error.code : "verification_failed";
  process.stdout.write(`${JSON.stringify({ schemaVersion: 1, status: "verification_failed", failureCode: code })}\n`);
  process.exitCode = 1;
}

function main() {
  let evidencePath;
  try {
    evidencePath = parseEvidenceArgument(process.argv.slice(2));
  } catch (error) {
    emitFailure(error);
    return;
  }

  const audit = run("cargo", ["audit", "--json"]);
  try {
    const resolvedEvidencePath = resolve(evidencePath);
    mkdirSync(dirname(resolvedEvidencePath), { recursive: true });
    writeFileSync(resolvedEvidencePath, audit.stdout, "utf8");
    if (audit.spawnError) throw verificationError("audit_command_failed");

    // Keep this index platform-unfiltered so host-built build dependencies also resolve.
    // Only cargo tree below selects the active graph for TARGET.
    const metadata = run("cargo", ["metadata", "--locked", "--format-version", "1"]);
    if (metadata.spawnError || metadata.status !== 0) throw verificationError("metadata_command_failed");

    const tree = run("cargo", [
      "tree",
      "--locked",
      "--workspace",
      "--target",
      TARGET,
      "--edges",
      "normal,build,dev",
      "--prefix",
      "none",
      "--format",
      "{p}",
    ]);
    if (tree.spawnError || tree.status !== 0) throw verificationError("cargo_tree_failed");

    const result = classifyDependencyAudit({
      auditText: audit.stdout,
      metadataText: metadata.stdout,
      treeText: tree.stdout,
      auditExitCode: audit.status,
    });
    process.stdout.write(`${JSON.stringify(result.summary)}\n`);
    process.exitCode = result.exitCode;
  } catch (error) {
    emitFailure(error);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main();
