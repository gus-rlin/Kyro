import { readdir, readFile } from 'node:fs/promises';
import { delimiter, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const documentPath = resolve(root, 'docs/backend/partie-1/openapi.v1.json');
const apiSourcePath = resolve(root, 'crates/kyro-api/src');
const extraSourcePaths = (process.env.KYRO_OPENAPI_ROUTE_SOURCE_DIRS ?? '')
  .split(delimiter)
  .filter(Boolean)
  .map((path) => resolve(path));

function fail(message) {
  process.stderr.write(`OpenAPI check failed: ${message}\n`);
  process.exitCode = 1;
}

function resolveReference(document, reference) {
  if (!reference.startsWith('#/')) throw new Error(`unsupported reference ${reference}`);
  return reference.slice(2).split('/').reduce((value, token) => {
    const key = token.replaceAll('~1', '/').replaceAll('~0', '~');
    if (value === null || typeof value !== 'object' || !(key in value)) {
      throw new Error(`unresolved reference ${reference}`);
    }
    return value[key];
  }, document);
}

function routeCallFor(source, path) {
  const routePattern = /\.route\s*\(\s*("(?:\\.|[^"\\])*")\s*,/g;
  for (const match of source.matchAll(routePattern)) {
    if (JSON.parse(match[1]) !== path) continue;
    const openingParen = source.indexOf('(', match.index);
    let depth = 0;
    let inString = false;
    let escaped = false;
    for (let index = openingParen; index < source.length; index += 1) {
      const character = source[index];
      if (inString) {
        if (escaped) escaped = false;
        else if (character === '\\') escaped = true;
        else if (character === '"') inString = false;
        continue;
      }
      if (character === '"') inString = true;
      else if (character === '(') depth += 1;
      else if (character === ')') {
        depth -= 1;
        if (depth === 0) return source.slice(openingParen, index + 1);
      }
    }
  }
  return null;
}

try {
  const document = JSON.parse(await readFile(documentPath, 'utf8'));
  if (document.openapi !== '3.1.0' || document.info?.version !== '1.0.0') {
    throw new Error('expected OpenAPI 3.1.0 and API version 1.0.0');
  }
  if (!document.info?.title || !document.info?.description) {
    throw new Error('the API title and description are required');
  }
  if (!document.paths || Object.keys(document.paths).length < 20) {
    throw new Error('the versioned API path inventory is incomplete');
  }

  const supportedMethods = new Set(['get', 'post', 'put', 'patch', 'delete']);
  const operationIds = new Set();
  let operationCount = 0;
  const apiSources = await Promise.all(
    (await Promise.all([apiSourcePath, ...extraSourcePaths].map(async (directory) =>
      (await readdir(directory))
        .filter((name) => name.endsWith('.rs'))
        .map((name) => resolve(directory, name)),
    ))).flat().map((path) => readFile(path, 'utf8')),
  );
  for (const [path, pathItem] of Object.entries(document.paths)) {
    const pathWithoutParameters = path.replace(/\{[^{}]+\}/g, '');
    if (!path.startsWith('/') || /[{}]/.test(pathWithoutParameters)) {
      throw new Error(`invalid OpenAPI path template: ${path}`);
    }
    const routeCalls = apiSources.map((source) => routeCallFor(source, path)).filter(Boolean);
    if (routeCalls.length === 0) throw new Error(`documented route is missing from the Axum sources: ${path}`);
    const pathParameters = (pathItem.parameters ?? []).map((parameter) =>
      typeof parameter.$ref === 'string' ? resolveReference(document, parameter.$ref) : parameter,
    );
    const templateParameters = [...path.matchAll(/\{([^{}]+)\}/g)].map((match) => match[1]);
    const operations = Object.entries(pathItem).filter(([method]) => method !== 'parameters');
    if (operations.length === 0) throw new Error(`route has no operations: ${path}`);
    for (const [method, operation] of operations) {
      if (!supportedMethods.has(method)) {
        throw new Error(`unsupported HTTP operation ${method} on ${path}`);
      }
      if (!operation.operationId || !operation.responses || Object.keys(operation.responses).length === 0) {
        throw new Error(`operation metadata is incomplete: ${method.toUpperCase()} ${path}`);
      }
      if (operationIds.has(operation.operationId)) {
        throw new Error(`duplicate operationId ${operation.operationId}`);
      }
      if (!routeCalls.some((call) => new RegExp(`\\b${method}\\s*\\(`).test(call))) {
        throw new Error(`documented method is missing from the Axum route: ${method.toUpperCase()} ${path}`);
      }
      operationIds.add(operation.operationId);
      operationCount += 1;

      const parameters = [
        ...pathParameters,
        ...(operation.parameters ?? []).map((parameter) =>
          typeof parameter.$ref === 'string' ? resolveReference(document, parameter.$ref) : parameter,
        ),
      ];
      for (const parameter of parameters) {
        if (!parameter.name || !['path', 'query', 'header', 'cookie'].includes(parameter.in)) {
          throw new Error(`invalid parameter on ${method.toUpperCase()} ${path}`);
        }
        if (parameter.in === 'path' && parameter.required !== true) {
          throw new Error(`path parameter ${parameter.name} must be required`);
        }
      }
      for (const name of templateParameters) {
        if (!parameters.some((parameter) => parameter.in === 'path' && parameter.name === name)) {
          throw new Error(`path parameter ${name} is undocumented on ${method.toUpperCase()} ${path}`);
        }
      }
      if (operation.requestBody) {
        const body = typeof operation.requestBody.$ref === 'string'
          ? resolveReference(document, operation.requestBody.$ref)
          : operation.requestBody;
        if (!body.content || Object.keys(body.content).length === 0) {
          throw new Error(`request body has no media type on ${method.toUpperCase()} ${path}`);
        }
      }
      for (const [status, responseRef] of Object.entries(operation.responses)) {
        if (status !== 'default' && !/^[1-5][0-9]{2}$/.test(status)) {
          throw new Error(`invalid response status ${status} on ${method.toUpperCase()} ${path}`);
        }
        const response = typeof responseRef.$ref === 'string'
          ? resolveReference(document, responseRef.$ref)
          : responseRef;
        if (!response.description || typeof response.description !== 'string') {
          throw new Error(`response has no description on ${method.toUpperCase()} ${path} (${status})`);
        }
      }
      for (const requirement of operation.security ?? []) {
        for (const scheme of Object.keys(requirement)) {
          if (!document.components.securitySchemes?.[scheme]) {
            throw new Error(`security scheme ${scheme} is undefined on ${method.toUpperCase()} ${path}`);
          }
        }
      }
    }
  }

  const visit = (value) => {
    if (Array.isArray(value)) {
      for (const child of value) visit(child);
    } else if (value && typeof value === 'object') {
      if (typeof value.$ref === 'string') resolveReference(document, value.$ref);
      for (const child of Object.values(value)) visit(child);
    }
  };
  visit(document);

  const requiredSchemas = [
    'ErrorEnvelope', 'ProjectSnapshot', 'AppRevision', 'ApplyChangesResult', 'ChangeSet',
    'BudgetSnapshot', 'EffectRecordView', 'JobView', 'CapabilityGrantLimits',
  ];
  for (const name of requiredSchemas) {
    if (!document.components.schemas?.[name]) throw new Error(`missing concrete DTO schema ${name}`);
  }
  const rejectPermissiveSchemas = (value, parent = '') => {
    if (Array.isArray(value)) {
      value.forEach((child, index) => rejectPermissiveSchemas(child, `${parent}[${index}]`));
    } else if (value && typeof value === 'object') {
      if (value.additionalProperties === true) {
        throw new Error(`unbounded additionalProperties in ${parent || 'OpenAPI document'}`);
      }
      for (const [key, child] of Object.entries(value)) rejectPermissiveSchemas(child, `${parent}.${key}`);
    }
  };
  rejectPermissiveSchemas(document.components.schemas, 'components.schemas');

  const grantLimits = document.components.schemas.CapabilityGrantLimits;
  if (document.components.schemas.ChangeSet.properties.operations.maxItems !== 128) {
    throw new Error('ChangeSet.operations must match the runtime limit of 128');
  }
  const resources = document.components.schemas.CreateCapabilityGrantRequest.properties.resources;
  if (resources.minItems !== 1 || resources.maxItems !== 1) {
    throw new Error('capability grants require exactly one project resource');
  }
  const expectedGrantLimits = {
    max_job_attempts: [1, 3],
    max_job_ttl_secs: [10, 1800],
    max_model_input_bytes: [1, 1048576],
    max_model_output_tokens: [1, 1000000],
    max_changeset_operations: [1, 128],
  };
  if (
    grantLimits.type !== 'object'
    || grantLimits.additionalProperties !== false
    || JSON.stringify(Object.keys(grantLimits.properties ?? {}).sort())
      !== JSON.stringify(Object.keys(expectedGrantLimits).sort())
  ) {
    throw new Error('CapabilityGrantLimits must be a closed object with the five supported keys');
  }
  for (const [name, [minimum, maximum]] of Object.entries(expectedGrantLimits)) {
    const property = grantLimits.properties[name];
    if (property.type !== 'integer' || property.minimum !== minimum || property.maximum !== maximum) {
      throw new Error(`invalid bounds on CapabilityGrantLimits.${name}`);
    }
  }
  const grantResponse = document.components.schemas.CapabilityGrant;
  const grantRequest = document.components.schemas.CreateCapabilityGrantRequest;
  if (
    grantResponse?.properties?.limits?.$ref !== '#/components/schemas/CapabilityGrantLimits'
    || !grantResponse.required?.includes('limits')
    || grantRequest?.properties?.limits?.$ref !== '#/components/schemas/CapabilityGrantLimits'
    || grantRequest.required?.includes('limits')
  ) {
    throw new Error('capability grant limits must be an optional request and a required response DTO');
  }

  const operationParameters = (path, method) => {
    const pathItem = document.paths[path];
    const operation = pathItem?.[method];
    return [
      ...(pathItem?.parameters ?? []),
      ...(operation?.parameters ?? []),
    ].map((parameter) => typeof parameter.$ref === 'string'
      ? resolveReference(document, parameter.$ref)
      : parameter);
  };
  for (const [path, method] of [
    ['/v1/projects/{project_id}/changes', 'post'],
    ['/v1/projects/{project_id}/data-policy', 'put'],
    ['/v1/projects/{project_id}/limits', 'put'],
    ['/v1/projects/{project_id}/budget', 'put'],
  ]) {
    if (!operationParameters(path, method).some((parameter) => parameter.name === 'If-Match' && parameter.required === true)) {
      throw new Error(`missing required If-Match on ${method.toUpperCase()} ${path}`);
    }
  }
  const reconcilePath = '/v1/projects/{project_id}/effects/{effect_id}/reconcile';
  const reconcile = document.paths[reconcilePath]?.post;
  const reconcileParameters = operationParameters(reconcilePath, 'post');
  if (!reconcile || !reconcileParameters.some((parameter) => parameter.name === 'Idempotency-Key' && parameter.required === true)) {
    throw new Error('effect reconciliation must require Idempotency-Key');
  }
  if (reconcileParameters.some((parameter) => parameter.name === 'If-Match')) {
    throw new Error('effect reconciliation is independent of project revision If-Match');
  }
  if (reconcile.responses['202']?.$ref !== '#/components/responses/Job') {
    throw new Error('effect reconciliation must return the safe asynchronous Job DTO');
  }
  if (!document.paths['/v1/projects/{project_id}/events']?.get?.responses?.['410']) {
    throw new Error('the SSE route must document expired-history recovery');
  }

  process.stdout.write(
    JSON.stringify({ status: 'ok', version: document.info.version, pathCount: Object.keys(document.paths).length, operationCount }) + '\n',
  );
} catch (error) {
  fail(error instanceof Error ? error.message : 'invalid document');
}
