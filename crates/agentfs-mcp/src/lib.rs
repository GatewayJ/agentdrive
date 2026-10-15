//! Authenticated MCP transport for the shared AgentFS service facade.
mod client;
mod gateway;
mod server;
mod web;
pub use client::{Client, stdio_proxy};
pub use gateway::{GatewayConfig, router};
pub use server::{McpServer, tool_catalog};
