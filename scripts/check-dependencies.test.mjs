import assert from "node:assert/strict";
import test from "node:test";

import { classifyDependencyAudit } from "./check-dependencies.mjs";

const rootNames = ["kyro-api", "kyro-domain", "kyro-gateway", "kyro-store", "kyro-worker", "kyro-app", "kyro-factory"];
const rsaId = "registry+https://github.com/rust-lang/crates.io-index#rsa@0.9.10";
const registry = "registry+https://github.com/rust-lang/crates.io-index";

function metadata() {
  const roots = rootNames.map((name) => ({
    id: `path+file:///workspace/crates/${name}#${name}@0.1.0`,
    name,
    version: "0.1.0",
    source: null,
  }));
  const packages = [
    ...roots,
    { id: rsaId, name: "rsa", version: "0.9.10", source: registry },
  ];
  return {
    packages,
    workspace_members: roots.map(({ id }) => id),
    resolve: { nodes: packages.map(({ id }) => ({ id, deps: [] })) },
  };
}

function tree({ activeRsa = false } = {}) {
  return rootNames.map((name, index) => {
    const root = `${name} v0.1.0 (C:\\private\\workspace\\crates\\${name})`;
    return index === 0 && activeRsa ? `${root}\nrsa v0.9.10` : root;
  }).join("\n\n");
}

function auditDocument({ vulnerable = true } = {}) {
  const list = vulnerable
    ? [{
        advisory: { id: "RUSTSEC-2023-0071" },
        package: { name: "rsa", version: "0.9.10", source: registry },
      }]
    : [];
  return { vulnerabilities: { found: list.length > 0, count: list.length, list } };
}

function classify({
  audit = auditDocument(),
  metadataValue = metadata(),
  treeText = tree(),
  auditExitCode,
  metadataExitCode = 0,
  treeExitCode = 0,
} = {}) {
  const effectiveAuditExitCode = auditExitCode ?? (
    typeof audit === "object" && audit?.vulnerabilities?.list?.length > 0 ? 1 : 0
  );
  return classifyDependencyAudit({
    auditText: typeof audit === "string" ? audit : JSON.stringify(audit),
    metadataText: typeof metadataValue === "string" ? metadataValue : JSON.stringify(metadataValue),
    treeText,
    auditExitCode: effectiveAuditExitCode,
    metadataExitCode,
    treeExitCode,
  });
}

test("an active vulnerable package is refused", () => {
  const result = classify({ treeText: tree({ activeRsa: true }) });

  assert.equal(result.exitCode, 1);
  assert.equal(result.summary.status, "active_vulnerabilities_found");
  assert.deepEqual(result.summary.activeVulnerabilities, [
    { id: "RUSTSEC-2023-0071", package: "rsa", version: "0.9.10" },
  ]);
  assert.deepEqual(result.summary.inactiveLockAdvisories, []);
});

test("a locked but inactive advisory is classified without claiming cargo audit passed", () => {
  const result = classify();

  assert.equal(result.exitCode, 0);
  assert.equal(result.summary.status, "active_graph_clear_inactive_lock_advisories");
  assert.equal(result.summary.auditExitCode, 1);
  assert.deepEqual(result.summary.activeVulnerabilities, []);
  assert.deepEqual(result.summary.inactiveLockAdvisories, [
    { id: "RUSTSEC-2023-0071", package: "rsa", version: "0.9.10" },
  ]);
});

test("malformed audit JSON fails closed", () => {
  assert.throws(() => classify({ audit: "{invalid" }), (error) => error.code === "invalid_audit_json");
});

test("a missing Cargo resolution graph fails closed", () => {
  const incomplete = metadata();
  incomplete.resolve.nodes = [];

  assert.throws(
    () => classify({ metadataValue: incomplete }),
    (error) => error.code === "missing_metadata_graph",
  );
});

test("unparseable cargo tree output fails closed", () => {
  const malformedTree = `${tree()}\nnot-a-package`;
  assert.throws(
    () => classify({ treeText: malformedTree }),
    (error) => error.code === "invalid_cargo_tree",
  );
});

test("audit command failure fails closed even with a valid no-finding JSON document", () => {
  assert.throws(
    () => classify({ audit: auditDocument({ vulnerable: false }), auditExitCode: 2 }),
    (error) => error.code === "audit_command_failed",
  );
});

test("the public summary does not copy local workspace paths", () => {
  const result = classify();

  assert.equal(JSON.stringify(result.summary).includes("private"), false);
  assert.equal(JSON.stringify(result.summary).includes("C:\\"), false);
});
