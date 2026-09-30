// Request decoding for the SDK wire protocol: JSON, gzip (`compression=gzip-js`
// or `Content-Encoding: gzip` or gzip magic bytes), base64 (`compression=base64`)
// and form-encoded `data=` (POST body or GET query). A body that cannot be
// decoded is a plain 400; one over the limits is a 413. Nothing is stored
// from a request that fails here.

import { gunzipSync } from "fflate";

export const MAX_BODY_BYTES = 2 * 1024 * 1024;
export const MAX_DECODED_BYTES = 16 * 1024 * 1024;

export class DecodeError extends Error {
  status: 400 | 413;
  constructor(status: 400 | 413, message: string) {
    super(message);
    this.status = status;
  }
}

const isGzip = (b: Uint8Array) => b.length > 2 && b[0] === 0x1f && b[1] === 0x8b;

function gunzip(b: Uint8Array): Uint8Array {
  let out: Uint8Array;
  try {
    out = gunzipSync(b);
  } catch {
    throw new DecodeError(400, "The body is not valid gzip.");
  }
  if (out.length > MAX_DECODED_BYTES) throw new DecodeError(413, "The decoded body is too large.");
  return out;
}

function fromBase64(text: string): string {
  const clean = text.replace(/\s+/g, "");
  if (!/^[A-Za-z0-9+/_-]*={0,2}$/.test(clean)) throw new DecodeError(400, "The body is not valid base64.");
  const bytes = Buffer.from(clean.replace(/-/g, "+").replace(/_/g, "/"), "base64");
  if (bytes.length > MAX_DECODED_BYTES) throw new DecodeError(413, "The decoded body is too large.");
  return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
}

function parseJson(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    throw new DecodeError(400, "The body is not valid JSON.");
  }
}

// `data=` values may be JSON, base64 JSON, or (older SDKs) base64 with + turned to spaces.
function parseDataValue(value: string, compression: string | null): unknown {
  if (compression === "base64") return parseJson(fromBase64(value.replace(/ /g, "+")));
  const t = value.trim();
  if (t.startsWith("{") || t.startsWith("[")) return parseJson(t);
  return parseJson(fromBase64(t.replace(/ /g, "+")));
}

export interface RawRequest {
  method: string;
  url: URL;
  contentType: string | null;
  contentEncoding: string | null;
  body: Uint8Array;
}

export function decodePayload(req: RawRequest): unknown {
  const compression = req.url.searchParams.get("compression");
  if (req.method === "GET" || req.body.length === 0) {
    const data = req.url.searchParams.get("data");
    if (!data) throw new DecodeError(400, "No event data was sent.");
    return parseDataValue(data, compression);
  }
  if (req.body.length > MAX_BODY_BYTES) throw new DecodeError(413, "The body is too large.");
  let bytes = req.body;
  if (compression === "gzip-js" || compression === "gzip" || /gzip/i.test(req.contentEncoding ?? "") || isGzip(bytes)) {
    bytes = gunzip(bytes);
  } else if (bytes.length > MAX_DECODED_BYTES) {
    throw new DecodeError(413, "The body is too large.");
  }
  const text = new TextDecoder("utf-8").decode(bytes);
  const ct = (req.contentType ?? "").toLowerCase();
  if (ct.includes("application/x-www-form-urlencoded") || /^data=/.test(text)) {
    const data = new URLSearchParams(text).get("data");
    if (data === null) throw new DecodeError(400, "The form body has no data field.");
    return parseDataValue(data, compression);
  }
  if (compression === "base64") return parseJson(fromBase64(text));
  return parseJson(text);
}
