// format is JSON.stringify with 2-space indentation, except that any object or
// array that fits on one line within 100 columns is kept on one line.
export function format(value, indent = "") {
  const flat = JSON.stringify(value);
  if (flat === undefined || indent.length + flat.length <= 100 || value === null || typeof value !== "object") {
    return flat;
  }
  const inner = `${indent}  `;
  if (Array.isArray(value)) {
    return `[\n${value.map((v) => inner + format(v, inner)).join(",\n")}\n${indent}]`;
  }
  const entries = Object.entries(value).filter(([, v]) => v !== undefined);
  return `{\n${entries.map(([k, v]) => `${inner}${JSON.stringify(k)}: ${format(v, inner)}`).join(",\n")}\n${indent}}`;
}
