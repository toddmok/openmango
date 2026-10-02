import { EJSON } from "bson";

export function safePrintable(value: unknown) {
  if (value === undefined) return null;
  try {
    // Structured payloads back the editable result views, so use canonical EJSON to
    // preserve Int32/Int64/Double and every other BSON type exactly. Raw printed
    // output remains relaxed and human-readable in formatPrintValue below.
    return EJSON.serialize(value, { relaxed: false });
  } catch {
    // Fall back for values that cannot be represented as BSON.
  }
  try {
    return JSON.parse(JSON.stringify(value));
  } catch {
    try {
      return String(value);
    } catch {
      return null;
    }
  }
}

export function formatPrintValue(value: unknown, kind: "print" | "printjson"): string {
  const printable = value && typeof value === "object" && "printable" in value
    ? value.printable
    : value;
  if (printable === undefined) return "undefined";
  try {
    if (kind === "printjson") {
      return EJSON.stringify(printable, { relaxed: true, indent: 2 });
    }
    if (typeof printable === "string") return printable;
    return EJSON.stringify(printable, { relaxed: true });
  } catch {
    try {
      return JSON.stringify(printable, null, kind === "printjson" ? 2 : undefined);
    } catch {
      return String(printable);
    }
  }
}
