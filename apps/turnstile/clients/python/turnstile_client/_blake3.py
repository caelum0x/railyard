"""Pure-Python BLAKE3 (32-byte output), ported from the official reference
implementation (https://github.com/BLAKE3-team/BLAKE3, CC0 / Apache-2.0).

Mint's AITP ``op_hash`` is BLAKE3 over canonical JSON, and BLAKE3 is not in the
standard library. Payloads here are a few hundred bytes, so the reference
implementation is fast enough. If the optional ``blake3`` package is installed
it is used instead.
"""

from __future__ import annotations

_M32 = 0xFFFFFFFF
_BLOCK_LEN = 64
_CHUNK_LEN = 1024
_CHUNK_START = 1 << 0
_CHUNK_END = 1 << 1
_PARENT = 1 << 2
_ROOT = 1 << 3

_IV = (
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
    0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
)
_PERM = (2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8)


def _rotr(x: int, n: int) -> int:
    return ((x >> n) | (x << (32 - n))) & _M32


def _g(s: list, a: int, b: int, c: int, d: int, mx: int, my: int) -> None:
    s[a] = (s[a] + s[b] + mx) & _M32
    s[d] = _rotr(s[d] ^ s[a], 16)
    s[c] = (s[c] + s[d]) & _M32
    s[b] = _rotr(s[b] ^ s[c], 12)
    s[a] = (s[a] + s[b] + my) & _M32
    s[d] = _rotr(s[d] ^ s[a], 8)
    s[c] = (s[c] + s[d]) & _M32
    s[b] = _rotr(s[b] ^ s[c], 7)


def _compress(cv: list, block: list, counter: int, block_len: int, flags: int) -> list:
    s = list(cv[:8]) + list(_IV[:4]) + [counter & _M32, (counter >> 32) & _M32, block_len, flags]
    m = list(block)
    for r in range(7):
        _g(s, 0, 4, 8, 12, m[0], m[1])
        _g(s, 1, 5, 9, 13, m[2], m[3])
        _g(s, 2, 6, 10, 14, m[4], m[5])
        _g(s, 3, 7, 11, 15, m[6], m[7])
        _g(s, 0, 5, 10, 15, m[8], m[9])
        _g(s, 1, 6, 11, 12, m[10], m[11])
        _g(s, 2, 7, 8, 13, m[12], m[13])
        _g(s, 3, 4, 9, 14, m[14], m[15])
        if r < 6:
            m = [m[i] for i in _PERM]
    for i in range(8):
        s[i] ^= s[i + 8]
        s[i + 8] ^= cv[i]
    return s


def _words(block: bytes) -> list:
    block = block.ljust(_BLOCK_LEN, b"\0")
    return [int.from_bytes(block[i:i + 4], "little") for i in range(0, _BLOCK_LEN, 4)]


class _Output:
    def __init__(self, cv, block, counter, block_len, flags):
        self.cv, self.block, self.counter, self.block_len, self.flags = cv, block, counter, block_len, flags

    def chaining_value(self) -> list:
        return _compress(self.cv, self.block, self.counter, self.block_len, self.flags)[:8]

    def root_bytes(self) -> bytes:
        words = _compress(self.cv, self.block, 0, self.block_len, self.flags | _ROOT)
        return b"".join(w.to_bytes(4, "little") for w in words[:8])


class _ChunkState:
    def __init__(self, key: tuple, counter: int):
        self.cv = list(key)
        self.counter = counter
        self.buf = b""
        self.blocks_compressed = 0

    def length(self) -> int:
        return _BLOCK_LEN * self.blocks_compressed + len(self.buf)

    def _start_flag(self) -> int:
        return _CHUNK_START if self.blocks_compressed == 0 else 0

    def update(self, data: bytes) -> None:
        while data:
            if len(self.buf) == _BLOCK_LEN:
                self.cv = _compress(self.cv, _words(self.buf), self.counter, _BLOCK_LEN,
                                    self._start_flag())[:8]
                self.blocks_compressed += 1
                self.buf = b""
            take = min(_BLOCK_LEN - len(self.buf), len(data))
            self.buf += data[:take]
            data = data[take:]

    def output(self) -> _Output:
        return _Output(self.cv, _words(self.buf), self.counter, len(self.buf),
                       self._start_flag() | _CHUNK_END)


def _parent(left: list, right: list) -> _Output:
    return _Output(list(_IV), left + right, 0, _BLOCK_LEN, _PARENT)


def _blake3_reference(data: bytes) -> bytes:
    chunk = _ChunkState(_IV, 0)
    stack: list = []
    while data:
        if chunk.length() == _CHUNK_LEN:
            cv = chunk.output().chaining_value()
            total = chunk.counter + 1
            while total & 1 == 0:
                cv = _parent(stack.pop(), cv).chaining_value()
                total >>= 1
            stack.append(cv)
            chunk = _ChunkState(_IV, chunk.counter + 1)
        take = min(_CHUNK_LEN - chunk.length(), len(data))
        chunk.update(data[:take])
        data = data[take:]
    out = chunk.output()
    for cv in reversed(stack):
        out = _parent(cv, out.chaining_value())
    return out.root_bytes()


def blake3_digest(data: bytes) -> bytes:
    """The 32-byte BLAKE3 hash of ``data``."""
    try:  # pragma: no cover - optional accelerated path
        import blake3 as _native  # type: ignore

        return _native.blake3(data).digest()
    except ImportError:
        return _blake3_reference(bytes(data))
