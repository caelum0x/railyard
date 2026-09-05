//! Port of `config/tracing/*` — the generic Span/Tracer/TracerProvider
//! abstraction plus provider selection by environment variable.
//!
//! Faithful-port decisions:
//! * The `Span` API (`start_span` / `set_attribute` / `set_error` / `end`) maps
//!   1:1 onto the OpenTelemetry global tracer, which is a no-op until a provider
//!   is installed — exactly like the Go `EmptyTracer` default.
//! * Datadog is routed via OTLP (Datadog ingests OTLP), per the port brief, so
//!   there is a single OTLP export path rather than a bespoke DD tracer.

use std::env;

use opentelemetry::global;
use opentelemetry::trace::{Span as _, Status, Tracer as _};
use opentelemetry::KeyValue;

/// Which provider the environment selects (`TRACING_PROVIDER`, else
/// `DD_TRACE_ENABLED`, else `OTEL_EXPORTER_OTLP_ENDPOINT`, else none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracingProviderKind {
    OTel,
    Datadog,
    Empty,
}

/// A tracing span wrapping the OpenTelemetry global tracer's span.
pub struct Span {
    inner: opentelemetry::global::BoxedSpan,
}

impl Span {
    /// `SetAttribute(key, value)` for an integer value.
    pub fn set_attribute_i64(&mut self, key: &'static str, value: i64) {
        self.inner.set_attribute(KeyValue::new(key, value));
    }

    /// `SetAttribute(key, value)` for a string value.
    pub fn set_attribute_str(&mut self, key: &'static str, value: impl Into<String>) {
        self.inner.set_attribute(KeyValue::new(key, value.into()));
    }

    /// `SetError(err)`.
    pub fn set_error(&mut self, err: &dyn std::fmt::Display) {
        self.inner.set_status(Status::error(err.to_string()));
    }

    /// `End()`.
    pub fn end(mut self) {
        self.inner.end();
    }
}

/// `tracing.StartSpan(ctx, operationName)`.
pub fn start_span(operation_name: &str) -> Span {
    let tracer = global::tracer("rago");
    Span {
        inner: tracer.start(operation_name.to_string()),
    }
}

/// Handle to an installed provider so it can be shut down on exit.
pub struct TracerProvider {
    kind: TracingProviderKind,
    sdk: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl TracerProvider {
    pub fn kind(&self) -> TracingProviderKind {
        self.kind
    }

    /// `Stop()`.
    pub fn stop(&self) {
        if let Some(sdk) = &self.sdk {
            let _ = sdk.shutdown();
        }
    }
}

fn find_tracing_provider() -> TracingProviderKind {
    match env::var("TRACING_PROVIDER").unwrap_or_default().as_str() {
        "datadog" => TracingProviderKind::Datadog,
        "opentelemetry" => TracingProviderKind::OTel,
        _ => {
            if crate::utils::env::get_env_as_bool("DD_TRACE_ENABLED", false) {
                TracingProviderKind::Datadog
            } else if !env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap_or_default()
                .is_empty()
            {
                TracingProviderKind::OTel
            } else {
                TracingProviderKind::Empty
            }
        }
    }
}

/// `InitTracerProvider` — selects and installs the provider. For OTel/Datadog it
/// installs an OTLP exporter as the global provider; otherwise the global tracer
/// stays a no-op (the "empty" provider).
pub fn init_tracer_provider() -> TracerProvider {
    let kind = find_tracing_provider();

    match kind {
        TracingProviderKind::Empty => TracerProvider { kind, sdk: None },
        TracingProviderKind::OTel | TracingProviderKind::Datadog => match build_otlp_provider() {
            Ok(sdk) => {
                global::set_tracer_provider(sdk.clone());
                TracerProvider {
                    kind,
                    sdk: Some(sdk),
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to init OTLP tracer provider");
                TracerProvider {
                    kind: TracingProviderKind::Empty,
                    sdk: None,
                }
            }
        },
    }
}

fn build_otlp_provider() -> anyhow::Result<opentelemetry_sdk::trace::SdkTracerProvider> {
    use opentelemetry_otlp::WithExportConfig;

    let mut builder = opentelemetry_otlp::SpanExporter::builder().with_tonic();

    if let Ok(endpoint) = env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        if !endpoint.is_empty() {
            builder = builder.with_endpoint(endpoint);
        }
    }

    let exporter = builder.build()?;

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();

    Ok(provider)
}
