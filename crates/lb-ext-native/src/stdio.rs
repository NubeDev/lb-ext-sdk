//! The production entry point: serve the control wire on the process's real stdin/stdout.
//!
//! `serve_stdio(tools)` is what a native extension's `#[tokio::main] async fn main` calls — it hands
//! the child's inherited pipes to [`crate::serve::serve`]. The host (`lb-supervisor`) launched this
//! process with its stdio wired to the control channel, so stdin *is* the request stream and stdout
//! *is* the reply stream. Nothing else may write to stdout (it would corrupt the frame stream); logs
//! go to stderr.

use crate::serve::{serve, Tools};

/// Serve the sidecar control wire on stdin/stdout until the host sends `shutdown` or closes the pipe.
/// The one call a native extension's `main` makes after building its [`Tools`].
pub async fn serve_stdio<T: Tools>(tools: T) -> std::io::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    serve(stdin, stdout, tools).await
}
