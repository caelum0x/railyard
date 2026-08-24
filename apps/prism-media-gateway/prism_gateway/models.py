"""Canonical public request and response models for Prism Sx0."""

from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, Field, model_validator


class RoutingPreferences(BaseModel):
    fallback: Literal["none", "equivalent"] = "equivalent"
    max_cost_micros: int | None = Field(default=None, gt=0)
    sort: Literal["price", "latency"] = "price"
    provider_order: list[str] = Field(default_factory=list, max_length=20)


class GenerationRequest(BaseModel):
    model: str = Field(min_length=1, max_length=160)
    prompt: str = Field(min_length=1, max_length=8_000)
    modality: Literal["image", "video"] | None = None
    duration_seconds: int = Field(default=5, ge=1, le=60)
    image_url: str | None = Field(default=None, max_length=2_048)
    routing: RoutingPreferences = Field(default_factory=RoutingPreferences)

    @model_validator(mode="after")
    def require_image_for_i2v(self) -> "GenerationRequest":
        if self.model.endswith("-i2v") and not self.image_url:
            raise ValueError("image_url is required for image-to-video models")
        return self


class Attempt(BaseModel):
    model: str
    provider: str
    outcome: Literal["failed", "submitted", "completed"]


class GenerationResponse(BaseModel):
    id: str
    object: Literal["media.generation"] = "media.generation"
    status: Literal["queued", "running", "completed"]
    model: str
    provider: str
    modality: Literal["image", "video"]
    output_url: str = ""
    provider_ref: str = ""
    cost_micros: int = Field(ge=0)
    currency: Literal["USD"] = "USD"
    attempts: list[Attempt]


class ModelResponse(BaseModel):
    id: str
    object: Literal["model"] = "model"
    modality: Literal["image", "video"]
    provider: str
    price_micros: int
    unit: str
    description: str = ""


class ModelListResponse(BaseModel):
    object: Literal["list"] = "list"
    data: list[ModelResponse]
