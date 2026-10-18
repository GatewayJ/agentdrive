//! Authenticated MCP transport for the shared AgentDrive service facade.
mod client;
mod gateway;
mod server;
mod web;
mod workspace_view;
pub use client::{Client, stdio_proxy};
pub use gateway::{GatewayConfig, router};
pub use server::{McpServer, tool_catalog};
