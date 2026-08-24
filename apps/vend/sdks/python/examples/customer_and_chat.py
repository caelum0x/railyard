"""Exercise a deployed Vend instance; no fake transport is used."""

from __future__ import annotations

import os
import uuid

from vend import VendClient


def required(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"missing {name}")
    return value


vend = VendClient(
    required("VEND_BASE_URL"),
    provisioning_key=required("VEND_PROVISIONING_KEY"),
)
vend.login(
    required("VEND_TENANT_ID"),
    required("VEND_EMAIL"),
    required("VEND_PASSWORD"),
)
print(f"authenticated; visible apps={len(vend.list_apps())}")

completion = vend.chat_completions(
    {
        "model": required("VEND_MODEL"),
        "messages": [{"role": "user", "content": "Reply with exactly: Vend works"}],
        "temperature": 0,
        "max_tokens": 16,
    },
    idempotency_key=os.environ.get("VEND_IDEMPOTENCY_KEY", str(uuid.uuid4())),
)
print(completion.body)
print(completion.settlement)

if os.environ.get("VEND_RUN_STREAM") == "true":
    with vend.chat_completions_stream(
        {
            "model": required("VEND_MODEL"),
            "messages": [{"role": "user", "content": "Count from one to three"}],
            "temperature": 0,
            "max_tokens": 32,
        },
        idempotency_key=os.environ.get(
            "VEND_STREAM_IDEMPOTENCY_KEY", str(uuid.uuid4())
        ),
    ) as stream:
        for event in stream:
            print(event.data)

vend.logout()
