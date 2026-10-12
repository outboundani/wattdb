// The named-callback registry (decision D52). A scenario writes a callback or a
// JS-only value as {"$fn": "<name>", "args": [...]}, and the recorder replaces
// it with registry[name](...args). The Go test harness keeps a registry with the
// same names and the same semantics, so every entry here must stay small,
// deterministic and easy to mirror.

// resolvePath walks an ElectroDB attribute proxy along a path like "a.b[0].c".
function resolvePath(attrs, path) {
  let node = attrs;
  for (const part of path.match(/[^.[\]]+/g)) {
    node = node[/^\d+$/.test(part) ? Number(part) : part];
    if (node === undefined) {
      throw new Error(`unknown attribute path "${path}"`);
    }
  }
  return node;
}

// renderWhere turns an expression tree into an ElectroDB where/condition string.
//   ["and", e1, e2, ...] / ["or", e1, e2, ...] / ["not", e]
//   [op, "attr.path", ...values] for every ElectroDB where operation, e.g.
//   ["eq", "status", "active"], ["between", "n", 1, 5], ["exists", "tags"]
function renderWhere(attrs, op, expr) {
  const [head, ...rest] = expr;
  switch (head) {
    case "and":
    case "or":
      return rest
        .map((e) => `(${renderWhere(attrs, op, e)})`)
        .join(head === "and" ? " AND " : " OR ");
    case "not":
      return `NOT (${renderWhere(attrs, op, rest[0])})`;
    default: {
      if (typeof op[head] !== "function") {
        throw new Error(`unknown where operation "${head}"`);
      }
      const [path, ...values] = rest;
      return op[head](resolvePath(attrs, path), ...values);
    }
  }
}

function isString(v) {
  return typeof v === "string";
}

export const registry = {
  // Values JSON can't hold.
  undefined: () => undefined,
  regexp: (source, flags) => new RegExp(source, flags),
  jsSet: (values) => new Set(values),

  // default: returns a fixed value.
  const: (value) => () => value,

  // get/set transforms, (value, item) => value.
  upper: () => (v) => (isString(v) ? v.toUpperCase() : v),
  lower: () => (v) => (isString(v) ? v.toLowerCase() : v),
  prefix: (p) => (v) => (v === undefined ? v : `${p}${v}`),
  // join builds a value from sibling attributes; used with watch.
  join: (sep, ...attrs) => (_, item) => attrs.map((a) => item[a]).join(sep),

  // validate: truthy means valid; a throw is a validation error.
  minLength: (n) => (v) => v === undefined || String(v).length >= n,
  throws: (message) => () => {
    throw new Error(message);
  },

  // index condition: (attrs) => boolean.
  has: (attr) => (a) => a[attr] !== undefined,
  equals: (attr, value) => (a) => a[attr] === value,

  // where/condition callbacks: (attrs, op) => string.
  where: (expr) => (attrs, op) => renderWhere(attrs, op, expr),

  // data callbacks for update/patch/upsert: each statement is
  // [op, "attr.path", ...values], e.g. ["set", "n", 1], ["remove", "tags"].
  data: (...statements) => (attrs, op) => {
    for (const [name, path, ...values] of statements) {
      op[name](resolvePath(attrs, path), ...values);
    }
  },
};

// resolve replaces every {"$fn": ...} in a JSON tree, leaving the input intact.
// tx is supplied by the recorder so transaction callbacks can build chains.
export function resolve(node, extra = {}) {
  if (Array.isArray(node)) {
    return node.map((n) => resolve(n, extra));
  }
  if (node === null || typeof node !== "object") {
    return node;
  }
  if (Object.hasOwn(node, "$fn")) {
    const fn = extra[node.$fn] ?? registry[node.$fn];
    if (!fn) {
      throw new Error(`unknown registry function "${node.$fn}"`);
    }
    return fn(...resolve(node.args ?? [], extra));
  }
  return Object.fromEntries(
    Object.entries(node).map(([k, v]) => [k, resolve(v, extra)]),
  );
}
