"""Sx0 routing, failover, execution, and exact-cost readback."""

from __future__ import annotations

import threading
from dataclasses import dataclass
from typing import Protocol
from uuid import uuid4

from app.domains.gateway.media_catalog import get_model, list_models
from app.domains.routing.policy import Candidate, RoutePolicy, select_route

from .auth import Principal
from .models import Attempt, GenerationRequest, GenerationResponse, ModelResponse


class PrismError(Exception):
    def __init__(self, message: str, status_code: int = 400) -> None:
        super().__init__(message)
        self.status_code = status_code


@dataclass(frozen=True)
class ExecutionOutcome:
    status: str
    output_url: str = ""
    cost_micros: int | None = None
    provider_ref: str = ""
    status_url: str = ""
    response_url: str = ""


class MediaExecutor(Protocol):
    def execute(
        self,
        candidate: Candidate,
        request: GenerationRequest,
        principal: Principal,
        correlation_id: str,
    ) -> ExecutionOutcome: ...


class AdapterExecutor:
    """Execute through ReelForge's production image/video adapters."""

    def execute(
        self,
        candidate: Candidate,
        request: GenerationRequest,
        principal: Principal,
        correlation_id: str,
    ) -> ExecutionOutcome:
        model = get_model(candidate.model_id)
        if model is None:
            raise RuntimeError(
                f"model {candidate.model_id!r} disappeared from the catalog"
            )
        byok = (
            principal.provider_api_key
            if principal.provider_key == candidate.provider_key
            else ""
        )
        if model.modality == "image":
            from app.domains.gateway.media_gateway import _generate_image_upstream

            url = _generate_image_upstream(
                model, prompt=request.prompt, session_http=None, api_key=byok
            )
            return ExecutionOutcome(
                "completed", output_url=url, cost_micros=candidate.est_cost_micros
            )

        from app.domains.gateway.media_gateway import _submit_video_upstream

        provider_ref, status_url, response_url = _submit_video_upstream(
            model,
            prompt=request.prompt,
            seconds=request.duration_seconds,
            aspect_ratio="9:16",
            image_url=request.image_url or "",
            session_http=None,
            api_key=byok,
        )
        return ExecutionOutcome(
            "queued",
            cost_micros=candidate.est_cost_micros,
            provider_ref=provider_ref,
            status_url=status_url,
            response_url=response_url,
        )

    def poll(
        self,
        record: GenerationResponse,
        handle: ExecutionOutcome,
        principal: Principal,
    ) -> ExecutionOutcome:
        from types import SimpleNamespace

        from app.domains.gateway.media_gateway import _poll_video_upstream

        terminal, output_url, failure = _poll_video_upstream(
            SimpleNamespace(
                model=record.model,
                provider=record.provider,
                provider_ref=handle.provider_ref,
                status_url=handle.status_url,
                response_url=handle.response_url,
            ),
            session_http=None,
            api_key=(
                principal.provider_api_key
                if principal.provider_key == record.provider
                else ""
            ),
        )
        if terminal and not output_url:
            raise RuntimeError(failure or "provider reported a failed generation")
        return ExecutionOutcome(
            "completed" if terminal else "running",
            output_url=output_url,
            cost_micros=record.cost_micros,
            provider_ref=handle.provider_ref,
            status_url=handle.status_url,
            response_url=handle.response_url,
        )


class GenerationStore:
    """Bounded process-local Sx0 readback store; durable storage lands in Sx1."""

    def __init__(self, max_records: int = 10_000) -> None:
        self._max_records = max_records
        self._records: dict[str, tuple[str, GenerationResponse, ExecutionOutcome]] = {}
        self._lock = threading.Lock()

    def put(
        self, owner: str, record: GenerationResponse, handle: ExecutionOutcome
    ) -> None:
        with self._lock:
            if len(self._records) >= self._max_records:
                self._records.pop(next(iter(self._records)))
            self._records[record.id] = (owner, record, handle)

    def get(
        self, owner: str, generation_id: str
    ) -> tuple[GenerationResponse, ExecutionOutcome] | None:
        with self._lock:
            value = self._records.get(generation_id)
            return (value[1], value[2]) if value and value[0] == owner else None


class PrismService:
    def __init__(
        self,
        executor: MediaExecutor | None = None,
        store: GenerationStore | None = None,
    ) -> None:
        self._executor = executor or AdapterExecutor()
        self._store = store or GenerationStore()

    @staticmethod
    def models(modality: str | None = None) -> list[ModelResponse]:
        if modality and modality not in {"image", "video"}:
            raise PrismError("modality must be image or video", 422)
        return [
            ModelResponse(
                id=model.id,
                modality=model.modality,
                provider=model.provider,
                price_micros=model.price_micros,
                unit=model.unit,
                description=model.description,
            )
            for model in list_models(modality)
            if model.modality in {"image", "video"}
        ]

    @staticmethod
    def _candidates(request: GenerationRequest) -> tuple[str, list[Candidate]]:
        primary = get_model(request.model)
        if primary is None or primary.modality not in {"image", "video"}:
            raise PrismError(f"unknown Sx0 image/video model {request.model!r}", 404)
        if request.modality and request.modality != primary.modality:
            raise PrismError(
                "requested modality does not match the selected model", 422
            )
        candidates = []
        for model in list_models(primary.modality):
            quantity = request.duration_seconds if model.modality == "video" else 1
            candidates.append(
                Candidate(
                    model_id=model.id,
                    provider_key=model.provider,
                    capability=model.modality,
                    est_cost_micros=model.price_micros * quantity,
                )
            )
        return primary.modality, candidates

    def generate(
        self, request: GenerationRequest, principal: Principal, correlation_id: str
    ) -> GenerationResponse:
        modality, candidates = self._candidates(request)
        route = select_route(
            request.model,
            candidates,
            RoutePolicy(
                fallback=request.routing.fallback,
                max_cost_micros=request.routing.max_cost_micros,
                provider_order=tuple(request.routing.provider_order),
                sort=request.routing.sort,
            ),
        )
        if not route:
            raise PrismError(
                "selected model is unavailable under the routing policy", 422
            )

        attempts: list[Attempt] = []
        failed_providers: set[str] = set()
        last_error = "no provider accepted the request"
        for candidate in route:
            if candidate.provider_key in failed_providers:
                continue
            try:
                outcome = self._executor.execute(
                    candidate, request, principal, correlation_id
                )
            except Exception as exc:
                failed_providers.add(candidate.provider_key)
                last_error = str(exc) or type(exc).__name__
                attempts.append(
                    Attempt(
                        model=candidate.model_id,
                        provider=candidate.provider_key,
                        outcome="failed",
                    )
                )
                continue

            public_status = (
                outcome.status
                if outcome.status in {"queued", "running", "completed"}
                else "queued"
            )
            exact_cost = (
                candidate.est_cost_micros
                if outcome.cost_micros is None
                else outcome.cost_micros
            )
            if (
                request.routing.max_cost_micros is not None
                and exact_cost > request.routing.max_cost_micros
            ):
                raise PrismError(
                    "provider-reported cost exceeded the caller's hard cap", 502
                )
            attempts.append(
                Attempt(
                    model=candidate.model_id,
                    provider=candidate.provider_key,
                    outcome="completed"
                    if public_status == "completed"
                    else "submitted",
                )
            )
            record = GenerationResponse(
                id=f"gen_{uuid4().hex}",
                status=public_status,
                model=candidate.model_id,
                provider=candidate.provider_key,
                modality=modality,
                output_url=outcome.output_url,
                provider_ref=outcome.provider_ref,
                cost_micros=max(0, int(exact_cost)),
                attempts=attempts,
            )
            self._store.put(principal.key_fingerprint, record, outcome)
            return record
        raise PrismError(f"all eligible providers failed: {last_error}", 502)

    def generation(
        self, principal: Principal, generation_id: str
    ) -> GenerationResponse:
        stored = self._store.get(principal.key_fingerprint, generation_id)
        if stored is None:
            raise PrismError("generation not found", 404)
        record, handle = stored
        poll = getattr(self._executor, "poll", None)
        if record.status in {"queued", "running"} and callable(poll):
            try:
                outcome = poll(record, handle, principal)
            except Exception as exc:
                raise PrismError(f"provider poll failed: {exc}", 502) from exc
            record = record.model_copy(
                update={"status": outcome.status, "output_url": outcome.output_url}
            )
            self._store.put(principal.key_fingerprint, record, outcome)
        return record
