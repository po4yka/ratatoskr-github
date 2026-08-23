/// Telemetry bootstrap failure.
#[derive(Debug, thiserror::Error)]
#[error("telemetry was already initialized")]
pub struct TelemetryError(#[source] Box<dyn std::error::Error + Send + Sync>);

/// Installs the process-wide structured telemetry subscriber once.
///
/// # Errors
///
/// Returns [`TelemetryError`] when another global subscriber is already installed.
pub fn init_telemetry() -> Result<(), TelemetryError> {
    tracing_subscriber::fmt()
        .json()
        .try_init()
        .map_err(TelemetryError)
}
