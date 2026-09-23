"""Ed25519 signing (RFC 8032) with no required third-party packages.

If the ``cryptography`` package is installed its constant-time implementation
is used. Otherwise a pure-Python port of the RFC 8032 section 6 reference code
signs. The pure path is NOT constant-time: prefer installing ``cryptography``
on shared hosts where timing side channels matter, or plug in your own signer
(for example a KMS) through the ``Signer`` protocol in ``client.py``.
"""

from __future__ import annotations

import hashlib

_P = 2**255 - 19
_Q = 2**252 + 27742317777372353535851937790883648493
_D = -121665 * pow(121666, -1, _P) % _P
_SQRT_M1 = pow(2, (_P - 1) // 4, _P)


def _add(a, b):
    x1 = (a[1] - a[0]) * (b[1] - b[0]) % _P
    x2 = (a[1] + a[0]) * (b[1] + b[0]) % _P
    c = 2 * a[3] * b[3] * _D % _P
    d = 2 * a[2] * b[2] % _P
    e, f, g, h = x2 - x1, d - c, d + c, x2 + x1
    return (e * f % _P, g * h % _P, f * g % _P, e * h % _P)


def _mul(s: int, point):
    acc = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            acc = _add(acc, point)
        point = _add(point, point)
        s >>= 1
    return acc


def _recover_x(y: int, sign: int) -> int:
    x2 = (y * y - 1) * pow(_D * y * y + 1, -1, _P)
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P != 0:
        x = x * _SQRT_M1 % _P
    if (x & 1) != sign:
        x = _P - x
    return x


_GY = 4 * pow(5, -1, _P) % _P
_GX = _recover_x(_GY, 0)
_G = (_GX, _GY, 1, _GX * _GY % _P)


def _compress(point) -> bytes:
    zinv = pow(point[2], -1, _P)
    x = point[0] * zinv % _P
    y = point[1] * zinv % _P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def _expand(seed: bytes):
    h = hashlib.sha512(seed).digest()
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    return a, h[32:]


def _pure_public(seed: bytes) -> bytes:
    a, _ = _expand(seed)
    return _compress(_mul(a, _G))


def _pure_sign(seed: bytes, message: bytes) -> bytes:
    a, prefix = _expand(seed)
    public = _compress(_mul(a, _G))
    r = int.from_bytes(hashlib.sha512(prefix + message).digest(), "little") % _Q
    big_r = _compress(_mul(r, _G))
    h = int.from_bytes(hashlib.sha512(big_r + public + message).digest(), "little") % _Q
    s = (r + h * a) % _Q
    return big_r + s.to_bytes(32, "little")


def public_key(seed: bytes) -> bytes:
    """The 32-byte Ed25519 public key for a 32-byte secret seed."""
    _check(seed)
    try:  # pragma: no cover - optional accelerated path
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

        key = Ed25519PrivateKey.from_private_bytes(seed)
        return key.public_key().public_bytes(
            serialization.Encoding.Raw, serialization.PublicFormat.Raw
        )
    except ImportError:
        return _pure_public(seed)


def sign(seed: bytes, message: bytes) -> bytes:
    """A 64-byte Ed25519 signature of ``message`` under ``seed``."""
    _check(seed)
    try:  # pragma: no cover - optional accelerated path
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

        return Ed25519PrivateKey.from_private_bytes(seed).sign(message)
    except ImportError:
        return _pure_sign(seed, message)


def _check(seed: bytes) -> None:
    if not isinstance(seed, (bytes, bytearray)) or len(seed) != 32:
        raise ValueError("an Ed25519 secret seed must be exactly 32 bytes")
