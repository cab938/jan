use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime},
};

use opentelemetry::{
    global,
    logs::{AnyValue, LogError, LogRecord as _, Logger as _, LoggerProvider as _, Severity},
    trace::TraceError,
    KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::runtime::Tokio;
use opentelemetry_sdk::{logs, resource::Resource, trace as sdktrace};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_log::LogLevel;
use tracing::{
    field::{Field, Visit},
    Event, Level,
};
use tracing_appender::{non_blocking::WorkerGuard, rolling};
use tracing_log::LogTracer;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{
    filter::EnvFilter,
    layer::{Context, SubscriberExt},
    Registry,
};

use crate::core::app::commands::get_jan_data_folder_path;

static TELEMETRY_STATE: OnceLock<TelemetryState> = OnceLock::new();

#[derive(Debug)]
struct TelemetryState {
    #[allow(dead_code)]
    tracer: sdktrace::Tracer,
    #[allow(dead_code)]
    file_guard: WorkerGuard,
    logger_provider: logs::LoggerProvider,
}

#[derive(Clone)]
struct WebviewLayer {
    emitter: Arc<dyn Fn(WebviewPayload) + Send + Sync>,
}

#[derive(Clone)]
struct OtelLogLayer {
    logger: Arc<logs::Logger>,
}

#[derive(Clone, Serialize)]
struct WebviewPayload {
    message: String,
    level: LogLevel,
}

impl WebviewLayer {
    fn new(emitter: Arc<dyn Fn(WebviewPayload) + Send + Sync>) -> Self {
        Self { emitter }
    }
}

impl OtelLogLayer {
    fn new(logger: logs::Logger) -> Self {
        Self {
            logger: Arc::new(logger),
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for WebviewLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);

        let message = match visitor.message {
            Some(msg) => msg,
            None => return,
        };

        let level = match *metadata.level() {
            Level::ERROR => LogLevel::Error,
            Level::WARN => LogLevel::Warn,
            Level::INFO => LogLevel::Info,
            Level::DEBUG => LogLevel::Debug,
            Level::TRACE => LogLevel::Trace,
        };

        (self.emitter)(WebviewPayload { message, level });
    }
}

impl<S> tracing_subscriber::Layer<S> for OtelLogLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);

        let message = visitor
            .message
            .unwrap_or_else(|| metadata.target().to_string());

        let mut log_record = self.logger.create_log_record();
        log_record.set_timestamp(SystemTime::now());
        log_record.set_body(AnyValue::String(message.into()));

        if let Some(severity) = level_to_severity(metadata.level()) {
            log_record.set_severity_number(severity);
            log_record.set_severity_text(severity.name().into());
        }

        if let Some(module) = metadata.module_path() {
            log_record.add_attribute("code.namespace", module);
        }
        if let Some(file) = metadata.file() {
            log_record.add_attribute("code.filepath", file);
        }
        if let Some(line) = metadata.line() {
            log_record.add_attribute("code.lineno", line as i64);
        }

        log_record.add_attribute("log.target", metadata.target());

        self.logger.emit(log_record);
    }
}

#[derive(Default)]
struct EventVisitor {
    message: Option<String>,
}

impl Visit for EventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }
}

fn level_to_severity(level: &Level) -> Option<Severity> {
    match *level {
        Level::TRACE => Some(Severity::Trace),
        Level::DEBUG => Some(Severity::Debug),
        Level::INFO => Some(Severity::Info),
        Level::WARN => Some(Severity::Warn),
        Level::ERROR => Some(Severity::Error),
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TelemetryError {
    #[error("failed to install log tracer: {0}")]
    LogTracer(#[from] log::SetLoggerError),
    #[error("failed to install tracing subscriber: {0}")]
    Subscriber(#[from] tracing::subscriber::SetGlobalDefaultError),
    #[error("failed to prepare telemetry output directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to configure OTLP exporter: {0}")]
    Trace(#[from] TraceError),
    #[error("failed to configure OTLP log exporter: {0}")]
    Log(#[from] LogError),
}

pub fn init<R: Runtime>(app_handle: &AppHandle<R>) -> Result<(), TelemetryError> {
    if TELEMETRY_STATE.get().is_some() {
        return Ok(());
    }
    let state = build_state(app_handle)?;
    let _ = TELEMETRY_STATE.set(state);
    Ok(())
}

pub fn shutdown() {
    if let Some(state) = TELEMETRY_STATE.get() {
        let _ = state.logger_provider.shutdown();
    }
    global::shutdown_tracer_provider();
}

fn build_state<R: Runtime>(app_handle: &AppHandle<R>) -> Result<TelemetryState, TelemetryError> {
    LogTracer::init()?;

    let log_dir = resolve_log_dir(app_handle)?;
    std::fs::create_dir_all(&log_dir)?;

    let file_appender = rolling::daily(log_dir, "app.log");
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);

    let stdout_layer = tracing_subscriber::fmt::layer().with_target(true).compact();

    let file_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_ansi(false)
        .with_writer(file_writer);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:4317".to_string());

    let trace_exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(endpoint.clone())
        .with_timeout(Duration::from_secs(5));

    let resource = Resource::new([
        KeyValue::new("service.name", "jan-desktop"),
        KeyValue::new("service.namespace", "jan"),
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
    ]);

    let trace_resource = resource.clone();
    let tracer = tauri::async_runtime::block_on(async move {
        opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_trace_config(sdktrace::Config::default().with_resource(trace_resource))
            .with_exporter(trace_exporter)
            .install_batch(Tokio)
    })?;

    let log_exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(endpoint)
        .with_timeout(Duration::from_secs(5));

    let log_resource = resource.clone();
    let logger_provider = tauri::async_runtime::block_on(async move {
        opentelemetry_otlp::new_pipeline()
            .logging()
            .with_log_config(logs::config().with_resource(log_resource))
            .with_exporter(log_exporter)
            .install_batch(Tokio)
    })?;

    let otel_layer: OpenTelemetryLayer<Registry, _> =
        tracing_opentelemetry::layer().with_tracer(tracer.clone());
    let otel_log_layer = OtelLogLayer::new(
        logger_provider
            .logger_builder("jan-desktop")
            .with_version(env!("CARGO_PKG_VERSION"))
            .build(),
    );
    let webview_layer = {
        let handle = app_handle.clone();
        let emitter = Arc::new(move |payload: WebviewPayload| {
            let _ = handle.emit("log://log", payload);
        });
        WebviewLayer::new(emitter)
    };

    let subscriber = tracing_subscriber::registry()
        .with(otel_layer)
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .with(webview_layer)
        .with(otel_log_layer);

    tracing::subscriber::set_global_default(subscriber)?;

    Ok(TelemetryState {
        tracer,
        file_guard,
        logger_provider,
    })
}

fn resolve_log_dir<R: Runtime>(app_handle: &AppHandle<R>) -> Result<PathBuf, TelemetryError> {
    let mut path = get_jan_data_folder_path(app_handle.clone());
    path.push("logs");
    Ok(path)
}
