FROM python:3.11-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PYTHONPATH=/workspace/apps/api:/workspace/apps/prism-media-gateway

WORKDIR /workspace
COPY apps/prism-media-gateway ./apps/prism-media-gateway
COPY apps/api/app ./apps/api/app
RUN pip install --no-cache-dir ./apps/prism-media-gateway \
    && useradd --create-home --uid 10001 prism

USER prism
EXPOSE 8090
CMD ["uvicorn", "prism_gateway.app:app", "--host", "0.0.0.0", "--port", "8090"]
