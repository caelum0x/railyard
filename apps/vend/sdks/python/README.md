# Vend Python SDK

Synchronous, standard-library-only client for Python 3.11+. Customer sessions
control apps and keys; provisioning keys call the metered inference plane.

```bash
python -m pip install -e apps/vend/sdks/python
```

```python
import os
import uuid
from vend import VendClient

vend = VendClient(os.environ["VEND_BASE_URL"])
vend.login(
    os.environ["VEND_TENANT_ID"],
    os.environ["VEND_EMAIL"],
    os.environ["VEND_PASSWORD"],
)
app = vend.list_apps()[0]
issued = vend.issue_key(app["id"], daily_cap_micros=5_000_000, environment="test")
vend.provisioning_key = issued["secret"]

result = vend.chat_completions(
    {
        "model": "tensorzero::model_name::openai::gpt-4o-mini",
        "messages": [{"role": "user", "content": "Hello"}],
    },
    idempotency_key=str(uuid.uuid4()),
)
print(result.body, result.settlement)

with vend.chat_completions_stream(
    {
        "model": "tensorzero::model_name::openai::gpt-4o-mini",
        "messages": [{"role": "user", "content": "Hello"}],
    },
    idempotency_key=str(uuid.uuid4()),
) as stream:
    for event in stream:
        print(event.data)
```

For browser-based external login, redirect to
`vend.oauth_start_url("github", tenant_id)` or
`vend.oauth_start_url("google", tenant_id)`. Vend admits only a verified email
that already has an invitation for that tenant; it is not an open signup path.
