/**
 * Pure TypeScript BLAKE3 (32-byte output), ported from the official reference
 * implementation (https://github.com/BLAKE3-team/BLAKE3, CC0 / Apache-2.0).
 *
 * Mint's AITP `op_hash` is BLAKE3 over canonical JSON; neither Node nor
 * WebCrypto ships BLAKE3, and the inputs are a few hundred bytes, so the
 * reference algorithm is plenty fast and keeps the client dependency-free.
 */

const BLOCK_LEN = 64;
const CHUNK_LEN = 1024;
const CHUNK_START = 1;
const CHUNK_END = 2;
const PARENT = 4;
const ROOT = 8;

const IV = Uint32Array.of(
  0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
  0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
);
const PERM = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];

function rotr(x: number, n: number): number {
  return ((x >>> n) | (x << (32 - n))) >>> 0;
}

function g(s: Uint32Array, a: number, b: number, c: number, d: number, mx: number, my: number): void {
  s[a] = (s[a] + s[b] + mx) >>> 0;
  s[d] = rotr(s[d] ^ s[a], 16);
  s[c] = (s[c] + s[d]) >>> 0;
  s[b] = rotr(s[b] ^ s[c], 12);
  s[a] = (s[a] + s[b] + my) >>> 0;
  s[d] = rotr(s[d] ^ s[a], 8);
  s[c] = (s[c] + s[d]) >>> 0;
  s[b] = rotr(s[b] ^ s[c], 7);
}

function compress(cv: Uint32Array, block: Uint32Array, counter: number, blockLen: number, flags: number): Uint32Array {
  const s = new Uint32Array(16);
  s.set(cv.subarray(0, 8), 0);
  s.set(IV.subarray(0, 4), 8);
  s[12] = counter >>> 0;
  s[13] = Math.floor(counter / 0x100000000) >>> 0;
  s[14] = blockLen;
  s[15] = flags;
  let m = Uint32Array.from(block);
  for (let r = 0; r < 7; r++) {
    g(s, 0, 4, 8, 12, m[0], m[1]);
    g(s, 1, 5, 9, 13, m[2], m[3]);
    g(s, 2, 6, 10, 14, m[4], m[5]);
    g(s, 3, 7, 11, 15, m[6], m[7]);
    g(s, 0, 5, 10, 15, m[8], m[9]);
    g(s, 1, 6, 11, 12, m[10], m[11]);
    g(s, 2, 7, 8, 13, m[12], m[13]);
    g(s, 3, 4, 9, 14, m[14], m[15]);
    if (r < 6) m = Uint32Array.from(PERM, (i) => m[i]);
  }
  for (let i = 0; i < 8; i++) {
    s[i] = (s[i] ^ s[i + 8]) >>> 0;
    s[i + 8] = (s[i + 8] ^ cv[i]) >>> 0;
  }
  return s;
}

function words(block: Uint8Array): Uint32Array {
  const padded = new Uint8Array(BLOCK_LEN);
  padded.set(block);
  const view = new DataView(padded.buffer);
  const out = new Uint32Array(16);
  for (let i = 0; i < 16; i++) out[i] = view.getUint32(i * 4, true);
  return out;
}

interface Output {
  cv: Uint32Array;
  block: Uint32Array;
  counter: number;
  blockLen: number;
  flags: number;
}

function chainingValue(o: Output): Uint32Array {
  return compress(o.cv, o.block, o.counter, o.blockLen, o.flags).subarray(0, 8);
}

function rootBytes(o: Output): Uint8Array<ArrayBuffer> {
  const w = compress(o.cv, o.block, 0, o.blockLen, o.flags | ROOT);
  const out = new Uint8Array(32);
  const view = new DataView(out.buffer);
  for (let i = 0; i < 8; i++) view.setUint32(i * 4, w[i], true);
  return out;
}

function parent(left: Uint32Array, right: Uint32Array): Output {
  const block = new Uint32Array(16);
  block.set(left, 0);
  block.set(right, 8);
  return { cv: IV, block, counter: 0, blockLen: BLOCK_LEN, flags: PARENT };
}

class ChunkState {
  cv: Uint32Array;
  readonly counter: number;
  buf: Uint8Array = new Uint8Array(0);
  blocksCompressed = 0;

  constructor(counter: number) {
    this.cv = Uint32Array.from(IV);
    this.counter = counter;
  }

  length(): number {
    return BLOCK_LEN * this.blocksCompressed + this.buf.length;
  }

  private startFlag(): number {
    return this.blocksCompressed === 0 ? CHUNK_START : 0;
  }

  update(data: Uint8Array): void {
    let input = data;
    while (input.length > 0) {
      if (this.buf.length === BLOCK_LEN) {
        this.cv = compress(this.cv, words(this.buf), this.counter, BLOCK_LEN, this.startFlag()).slice(0, 8);
        this.blocksCompressed += 1;
        this.buf = new Uint8Array(0);
      }
      const take = Math.min(BLOCK_LEN - this.buf.length, input.length);
      const next = new Uint8Array(this.buf.length + take);
      next.set(this.buf);
      next.set(input.subarray(0, take), this.buf.length);
      this.buf = next;
      input = input.subarray(take);
    }
  }

  output(): Output {
    return {
      cv: this.cv,
      block: words(this.buf),
      counter: this.counter,
      blockLen: this.buf.length,
      flags: this.startFlag() | CHUNK_END,
    };
  }
}

/** The 32-byte BLAKE3 hash of `data`. */
export function blake3(data: Uint8Array): Uint8Array<ArrayBuffer> {
  let chunk = new ChunkState(0);
  const stack: Uint32Array[] = [];
  let input = data;
  while (input.length > 0) {
    if (chunk.length() === CHUNK_LEN) {
      let cv = chainingValue(chunk.output()).slice();
      let total = chunk.counter + 1;
      while ((total & 1) === 0) {
        const left = stack.pop();
        if (!left) throw new Error("blake3: chaining-value stack underflow");
        cv = chainingValue(parent(left, cv)).slice();
        total >>>= 1;
      }
      stack.push(cv);
      chunk = new ChunkState(chunk.counter + 1);
    }
    const take = Math.min(CHUNK_LEN - chunk.length(), input.length);
    chunk.update(input.subarray(0, take));
    input = input.subarray(take);
  }
  let out = chunk.output();
  for (let i = stack.length - 1; i >= 0; i--) {
    out = parent(stack[i], chainingValue(out).slice());
  }
  return rootBytes(out);
}
