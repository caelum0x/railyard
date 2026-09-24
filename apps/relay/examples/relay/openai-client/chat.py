"""Call Relay with the official OpenAI Python SDK.

    pip install openai
    python chat.py

Relay must be running (docker compose -f docker-compose.quickstart.yml up).
Set RELAY_API_KEY only when the gateway has `[gateway.auth] enabled = true`.
"""

import os

from openai import OpenAI

BASE_URL = os.environ.get("RELAY_URL", "http://localhost:3000") + "/openai/v1"
# The quickstart gateway has auth off, so any non-empty string works here.
API_KEY = os.environ.get("RELAY_API_KEY", "relay-quickstart")

client = OpenAI(base_url=BASE_URL, api_key=API_KEY)

# 1. A named function. The model behind `chat` is chosen in tensorzero.toml,
#    so you can swap it without changing this code.
response = client.chat.completions.create(
    model="tensorzero::function_name::chat",
    messages=[{"role": "user", "content": "Say hello in five words."}],
)
print("function chat:", response.choices[0].message.content)

# 2. A named model with provider failover (OpenAI, then OpenRouter).
response = client.chat.completions.create(
    model="tensorzero::model_name::gpt-4o-mini",
    messages=[{"role": "user", "content": "Name one prime number."}],
)
print("model gpt-4o-mini:", response.choices[0].message.content)

# 3. Streaming works the same way.
stream = client.chat.completions.create(
    model="tensorzero::function_name::chat",
    messages=[{"role": "user", "content": "Count from 1 to 5."}],
    stream=True,
)
print("stream: ", end="")
for chunk in stream:
    if chunk.choices and chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="", flush=True)
print()

# 4. Embeddings.
embedding = client.embeddings.create(
    model="tensorzero::embedding_model_name::text-embedding-3-small",
    input="Relay is an open-source LLM gateway.",
)
print("embedding dimensions:", len(embedding.data[0].embedding))
