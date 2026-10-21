import type { Cell } from "./types";
import { structuredJsonForCopy } from "./structured";

export function utf8Bytes(value: string): number { return new TextEncoder().encode(value).length; }
export function clipped(value: string, limit = 4096): string {
  let bytes = 0, out = "";
  for (const ch of value) {
    const n = ch.codePointAt(0)!;
    const size = n < 0x80 ? 1 : n < 0x800 ? 2 : n < 0x10000 ? 3 : 4;
    if (bytes + size > limit) return out + "…";
    out += ch; bytes += size;
  }
  return out;
}
export function isTruncated(cell: Cell): boolean { return "truncated" in cell && cell.truncated; }
export function cellText(cell: Cell): string {
  if (cell.kind === "null") return "NULL";
  if (cell.kind === "boolean") return String(cell.value);
  if (cell.kind === "binary") return `${cell.byteCount} bytes · 0x${cell.previewHex}${cell.truncated ? "…" : ""}`;
  return cell.value;
}
export function cellForCopy(cell: Cell): string {
  if (isTruncated(cell)) throw new Error("Expand this value before copying it.");
  if (cell.kind === "json") return structuredJsonForCopy(cell.value);
  if (cell.kind === "binary") return `0x${cell.previewHex}`;
  return cellText(cell);
}
export function cellJson(cell: Cell): string {
  if (isTruncated(cell)) throw new Error("Full values are required to copy this row.");
  if (cell.kind === "null") return "null";
  if (cell.kind === "boolean") return String(cell.value);
  if (cell.kind === "number") {
    return /^[+-]?(?:NaN|Infinity)$/i.test(cell.value) ? JSON.stringify(cell.value) : cell.value;
  }
  if (cell.kind === "json") return structuredJsonForCopy(cell.value);
  if (cell.kind === "binary") return `{ "$binary_hex": ${JSON.stringify(cell.previewHex)}, "byteCount": ${JSON.stringify(cell.byteCount)} }`;
  return JSON.stringify(cell.value);
}
