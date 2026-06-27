pub mod install;
mod server;

pub use server::{CurrentBrowser, WebcatMcp};

use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};

use crate::error::{Error, Result};

pub async fn serve(addr: SocketAddr, mcp: WebcatMcp) -> Result<u16> {
    let service = StreamableHttpService::new(
        move || Ok(mcp.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("MCP bind {addr}: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| Error::Other(anyhow::anyhow!(e)))?
        .port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(port)
}
