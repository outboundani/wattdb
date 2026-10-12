// record.mjs runs every scenario through ElectroDB and writes golden vectors.
//
//   pnpm record                                   generic fixtures
//   pnpm record --scenarios DIR --out DIR [name]  any other scenario set
//
// See README.md for the scenario and vector formats.

import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { Entity, Service } from "electrodb";
import * as lib from "@aws-sdk/lib-dynamodb";
import { convertToAttr } from "@aws-sdk/util-dynamodb";
import { format } from "./format.mjs";
import { resolve } from "./registry.mjs";
import { FakeTransport, newDocumentClient } from "./transport.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const electrodbVersion = createRequire(import.meta.url)("electrodb/package.json").version;

const { values: flags, positionals: only } = parseArgs({
  allowPositionals: true,
  options: {
    scenarios: { type: "string", default: path.join(here, "scenarios") },
    out: { type: "string", default: path.join(here, "../../testdata/vectors") },
  },
});

// applyChain calls each [method, ...args] in turn, starting from target.
// A dotted method such as "query.byStatus" walks properties before the call.
// A step with no arguments may also name a property, such as ["scan"].
function applyChain(target, chain) {
  let obj = target;
  for (const [method, ...args] of chain) {
    const parts = method.split(".");
    const name = parts.pop();
    let owner = obj;
    for (const part of parts) {
      owner = owner?.[part];
    }
    if (typeof owner?.[name] === "function") {
      obj = owner[name](...args);
    } else if (args.length === 0 && owner?.[name] !== undefined) {
      obj = owner[name];
    } else {
      throw new Error(`"${method}" is not a method`);
    }
  }
  return obj;
}

// tx builds a transaction callback from ops shaped like
// {"entity": "user", "chain": [...], "commit": {...}}.
const txRegistry = {
  tx: (...ops) => (entities) =>
    ops.map((op) => applyChain(entities[op.entity], op.chain).commit(op.commit)),
};

function isChain(x) {
  return x !== null && typeof x === "object" && typeof x.go === "function" && typeof x.params === "function";
}

// encode converts a JS value to wire AttributeValue JSON (decision D53).
function encode(value) {
  if (value === undefined) {
    return undefined;
  }
  return convertToAttr(value, { removeUndefinedValues: true, convertClassInstanceToMap: true });
}

function errorJSON(err) {
  const out = { name: err?.name, message: err?.message ?? String(err) };
  if (err?.code !== undefined) out.code = err.code;
  if (Array.isArray(err?.fields)) {
    out.fields = err.fields.map((f) => ({
      field: f.field,
      index: f.index,
      reason: f.reason,
      ...(f.cause !== undefined && { cause: errorJSON(f.cause) }),
    }));
  }
  if (err?.cause !== undefined) out.cause = errorJSON(err.cause);
  return out;
}

// commandFor picks the lib-dynamodb command that ElectroDB would send params with.
function commandFor(params, chain) {
  if (params.TransactItems) {
    return params.TransactItems[0]?.Get ? lib.TransactGetCommand : lib.TransactWriteCommand;
  }
  if (params.RequestItems) {
    return Object.values(params.RequestItems)[0]?.Keys ? lib.BatchGetCommand : lib.BatchWriteCommand;
  }
  if (params.KeyConditionExpression) return lib.QueryCommand;
  if (params.UpdateExpression) return lib.UpdateCommand;
  if (params.Item) return lib.PutCommand;
  if (params.Key) {
    return ["delete", "remove"].includes(chain[0][0]) ? lib.DeleteCommand : lib.GetCommand;
  }
  return lib.ScanCommand;
}

async function recordCase(env, c) {
  const { transport, client, entities, services } = env;
  const target = c.entity !== undefined ? entities[c.entity] : services[c.service];
  if (!target) {
    throw new Error(`case "${c.name}": unknown entity or service`);
  }
  const build = () => applyChain(target, resolve(c.chain, txRegistry));

  const recorded = {};
  const out = { ...c, recorded };
  let chain;
  try {
    chain = build();
  } catch (err) {
    recorded.error = errorJSON(err);
    return out;
  }

  // Chains that don't end in a request builder (parse, conversions, ...) are
  // recorded as a plain value.
  if (!isChain(chain)) {
    recorded.value = encode(chain);
    return out;
  }

  if (c.params !== false) {
    // The params are sent through the real DocumentClient so they're recorded
    // exactly as they'd go over the wire.
    try {
      const params = build().params(resolve(c.params ?? {}));
      transport.reset([]);
      for (const p of Array.isArray(params) ? params : [params]) {
        const Command = commandFor(p, c.chain);
        await client.send(new Command(p));
      }
      recorded.params = transport.requests;
    } catch (err) {
      recorded.params = { error: errorJSON(err) };
    }
  }

  if (c.go !== false) {
    transport.reset(c.responses ?? []);
    const go = {};
    try {
      go.result = encode(await build().go(resolve(c.go ?? {})));
    } catch (err) {
      go.error = errorJSON(err);
    }
    go.requests = transport.requests;
    if (transport.responses.length > 0 && !go.error) {
      throw new Error(`case "${c.name}": ${transport.responses.length} scripted responses were never used`);
    }
    recorded.go = go;
  }
  return out;
}

async function recordScenario(file) {
  const scenario = JSON.parse(fs.readFileSync(file, "utf8"));
  const transport = new FakeTransport();
  const client = newDocumentClient(transport);
  const table = scenario.table ?? "table";

  const entities = {};
  for (const [name, def] of Object.entries(scenario.entities ?? {})) {
    const schema = resolve({ model: def.model, attributes: def.attributes, indexes: def.indexes });
    entities[name] = new Entity(schema, { table, client, ...resolve(def.config ?? {}) });
  }
  const services = {};
  for (const [name, def] of Object.entries(scenario.services ?? {})) {
    const members = Object.fromEntries(def.entities.map((e) => [e, entities[e]]));
    services[name] = new Service(members, { table, client, ...resolve(def.config ?? {}) });
  }

  const env = { transport, client, entities, services };
  const cases = [];
  for (const c of scenario.cases ?? []) {
    cases.push(await recordCase(env, c));
  }
  return { electrodb: electrodbVersion, ...scenario, cases };
}

const files = fs
  .readdirSync(flags.scenarios)
  .filter((f) => f.endsWith(".json"))
  .filter((f) => only.length === 0 || only.includes(path.basename(f, ".json")))
  .sort();

fs.mkdirSync(flags.out, { recursive: true });
for (const f of files) {
  const vector = await recordScenario(path.join(flags.scenarios, f));
  fs.writeFileSync(path.join(flags.out, f), `${format(vector)}\n`);
  console.log(`recorded ${f}: ${vector.cases.length} cases`);
}
