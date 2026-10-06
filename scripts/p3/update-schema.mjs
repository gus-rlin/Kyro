import { writeFileSync } from 'node:fs';

// Native wire v2. Domain validation remains authoritative after schema validation.
const text = (maxLength = 128) => ({ type: 'string', maxLength });
const object = properties => ({ type: 'object', properties, required: Object.keys(properties), additionalProperties: false });
const array = (items, maxItems, minItems = 0) => ({ type: 'array', items, maxItems, ...(minItems ? { minItems } : {}) });
const union = (...anyOf) => ({ anyOf });
const literal = value => ({ type: typeof value, const: value, ...(typeof value === 'string' ? { maxLength: 128 } : {}) });
const scalar = () => [text(16384), { type: 'number' }, { type: 'boolean' }, { type: 'null' }];
function value(depth) {
  if (!depth) return union(...scalar());
  const children = value(depth - 1);
  return union(...scalar(), {
    type: 'object', properties: {}, required: [],
    additionalProperties: children, maxProperties: 128,
  }, array(children, 256));
}
const jsonValue = value(2);
const properties = {
  ...object({ version: text(64), configuration: { type: 'object', properties: {}, required: [], additionalProperties: jsonValue, maxProperties: 128 },
    depends_on: array(text(), 32), bindings: { type: 'object', properties: {}, required: [], additionalProperties: text(), maxProperties: 16 } }),
};
const node = object({
  id: { ...text(), description: 'Exact application node ID from task.writes node resources; not a catalogue component ID.' },
  kind: { ...text(64), description: 'Exact catalogue component ID from task.components, for example B031. Never the literal node or component.' },
  properties,
});
const operation = union(
  object({ op: literal('add_node'), node }),
  object({ op: literal('set_property'), node_id: text(), key: text(), value: jsonValue }),
  object({ op: literal('remove_node'), node_id: text() }),
  object({ op: literal('set_preference'), key: text(), value: jsonValue }),
);
const changes = object({ operations: array(operation, 128, 1) });
const resource = union(
  object({ kind: literal('node'), id: text() }),
  object({ kind: literal('property'), id: text(), key: text() }),
  object({ kind: literal('preference'), key: text() }),
);
const task = object({
  id: text(), objective: text(2048),
  components: array(object({ id: text(64), version: text(64) }), 16, 1),
  reads: array(resource, 64), writes: { ...array(resource, 32, 1), description: 'Declare every writable resource explicitly. A new node requires {kind:node,id:the_new_node_id}. Reads may be empty; writes must not be empty.' },
  dependencies: array(text(), 32), invariants: array(text(), 64),
  max_attempts: { type: 'integer', minimum: 1, maximum: 3 },
  deterministic: union({ type: 'null' }, changes),
});
const schema = object({ contract: union(
  object({ objective: text(8192), tasks: array(task, 32, 1), missing_capabilities: array(text(2048), 32) }),
  object({ task_id: text(), changes, limitations: array(text(2048), 32) }),
  object({ candidate_digest: text(64), approved: { type: 'boolean' }, findings: array(text(2048), 64) }),
) });
writeFileSync(new URL('../../crates/kyro-agents/src/contract-v2.schema.json', import.meta.url), JSON.stringify(schema) + '\n');
console.log(JSON.stringify({ bytes: Buffer.byteLength(JSON.stringify(schema)) }));
