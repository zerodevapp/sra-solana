export type JsonObject = Record<string, unknown>;

export function objectAt(value: unknown, path: string): JsonObject {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${path} must be an object`);
  }
  return value as JsonObject;
}

export function exactFields(
  value: JsonObject,
  fields: readonly string[],
  path: string
) {
  knownFields(value, fields, path);
  for (const field of fields) {
    if (!(field in value)) throw new Error(`${path}.${field} is required`);
  }
}

export function knownFields(
  value: JsonObject,
  fields: readonly string[],
  path: string
) {
  for (const field of Object.keys(value)) {
    if (!fields.includes(field)) {
      throw new Error(`${path} has unknown field ${field}`);
    }
  }
}

export function stringAt(value: unknown, path: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${path} must be a non-empty string`);
  }
  return value;
}

export function integerAt(
  value: unknown,
  path: string,
  min: number,
  max: number
) {
  const parsed = value as number;
  if (!Number.isInteger(value) || parsed < min || parsed > max) {
    throw new Error(`${path} must be an integer between ${min} and ${max}`);
  }
  return parsed;
}

export function decimalAt(
  value: unknown,
  path: string,
  positive = false
): string {
  const text = stringAt(value, path);
  if (!/^(0|[1-9][0-9]*)$/.test(text) || (positive && text === "0")) {
    throw new Error(
      `${path} must be a ${positive ? "positive " : ""}decimal integer`
    );
  }
  if (BigInt(text) > 18_446_744_073_709_551_615n) {
    throw new Error(`${path} exceeds u64`);
  }
  return text;
}

export function hexAt(value: unknown, path: string, bytes: number): string {
  const text = stringAt(value, path);
  if (!new RegExp(`^0x[0-9a-fA-F]{${bytes * 2}}$`).test(text)) {
    throw new Error(`${path} must be 0x-prefixed ${bytes}-byte hex`);
  }
  return text;
}

export function nonzeroHexAt(value: unknown, path: string, bytes: number) {
  const text = hexAt(value, path, bytes);
  if (text.toLowerCase() === `0x${"00".repeat(bytes)}`) {
    throw new Error(`${path} must not be zero`);
  }
  return text;
}

export function hexBlobAt(value: unknown, path: string): string {
  const text = stringAt(value, path);
  if (!/^0x(?:[0-9a-fA-F]{2})+$/.test(text)) {
    throw new Error(`${path} must be 0x-prefixed even-length hex`);
  }
  return text;
}
