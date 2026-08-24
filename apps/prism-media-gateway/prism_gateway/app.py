"""FastAPI entrypoint for Prism's Sx0 image/video surface."""

from __future__ import annotations

from uuid import uuid4

from fastapi import Depends, FastAPI, Header, Query, Request
from fastapi.responses import JSONResponse

from .auth import ApiKeyAuthenticator, Principal
from .models import GenerationRequest, GenerationResponse, ModelListResponse
from .service import PrismError, PrismService


def create_app(
    service: PrismService | None = None,
    authenticator: ApiKeyAuthenticator | None = None,
) -> FastAPI:
    gateway = service or PrismService()
    auth = authenticator or ApiKeyAuthenticator()
    instance = FastAPI(title="Prism Media Gateway", version="0.1.0")

    def principal(request: Request) -> Principal:
        return auth.authenticate(request)

    @instance.exception_handler(PrismError)
    async def prism_error_handler(_request: Request, exc: PrismError):
        return JSONResponse(
            status_code=exc.status_code, content={"error": {"message": str(exc)}}
        )

    @instance.get("/healthz", include_in_schema=False)
    def healthz():
        return {"status": "ok"}

    @instance.get("/v1/models", response_model=ModelListResponse)
    def models(modality: str | None = Query(default=None)):
        return ModelListResponse(data=gateway.models(modality))

    @instance.post("/v1/media/generations", response_model=GenerationResponse)
    def generate(
        body: GenerationRequest,
        actor: Principal = Depends(principal),
        correlation_id: str | None = Header(default=None, alias="X-Correlation-ID"),
    ):
        return gateway.generate(body, actor, correlation_id or uuid4().hex)

    @instance.get("/v1/generation", response_model=GenerationResponse)
    def generation(
        generation_id: str = Query(alias="id", min_length=1),
        actor: Principal = Depends(principal),
    ):
        return gateway.generation(actor, generation_id)

    return instance


app = create_app()
