//! OpenCSG Codes HTTP storage for AgentDrive objects and conditional references.
mod http;
mod objects;
mod refs;

use agentdrive_config::OpenCsgConfig;
use agentdrive_model::*;
use http::Repository;
use reqwest::Method;
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, OnceCell};

pub struct OpenCsgBackend {
    repository: Repository,
    prefix: String,
    verified: OnceCell<()>,
    writes: Mutex<()>,
}

impl std::fmt::Debug for OpenCsgBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCsgBackend")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl OpenCsgBackend {
    pub fn new(config: OpenCsgConfig) -> Result<Arc<Self>> {
        config.validate()?;
        Ok(Arc::new(Self {
            repository: Repository::new(&config)?,
            prefix: config.prefix,
            verified: OnceCell::new(),
            writes: Mutex::new(()),
        }))
    }

    pub async fn check_connection(&self) -> Result<()> {
        self.verified
            .get_or_try_init(|| self.probe())
            .await
            .map(|_| ())
    }

    async fn probe(&self) -> Result<()> {
        let path = format!("{}/checks/{}.json", self.prefix, OperationId::new());
        let create_path = format!("{path}.create");
        let result = async {
            self.create(&path, b"initial").await?;
            let old = self.update_probe(&path).await?;
            let current = self.repository.head().await?;
            if self.repository.read(&path, &current).await?.as_deref() != Some(b"updated") {
                return Err(Error::integrity(
                    "OpenCSG connection check could not read the updated file",
                ));
            }
            // A successful stale write would invalidate the engine's ownership and publication guards.
            let stale = self
                .repository
                .write(Method::PUT, &path, b"stale", &old)
                .await;
            let current = self.repository.head().await?;
            if self.repository.read(&path, &current).await?.as_deref() != Some(b"updated") {
                return Err(Error::new(
                    ErrorCode::Unsupported,
                    "OpenCSG must reject file updates with a stale start_sha",
                ));
            }
            match stale {
                Err(error) if error.code == ErrorCode::Conflict => {}
                Err(error) => return Err(error),
                Ok(()) => {
                    return Err(Error::new(
                        ErrorCode::Unsupported,
                        "OpenCSG did not report the stale update as rejected",
                    ));
                }
            }
            let stale_create = self
                .repository
                .write(Method::POST, &create_path, b"stale", &old)
                .await;
            let head = self.repository.head().await?;
            if self.repository.read(&create_path, &head).await?.is_some()
                || self.repository.read(&path, &head).await?.as_deref() != Some(b"updated")
            {
                return Err(Error::new(
                    ErrorCode::Unsupported,
                    "OpenCSG must reject file creation with a stale start_sha",
                ));
            }
            match stale_create {
                Err(error) if error.code == ErrorCode::Conflict => {}
                Err(error) => return Err(error),
                Ok(()) => {
                    return Err(Error::new(
                        ErrorCode::Unsupported,
                        "OpenCSG did not report stale creation as rejected",
                    ));
                }
            }
            for attempt in 0..8 {
                let head = self.repository.head().await?;
                let duplicate = self
                    .repository
                    .write(Method::POST, &path, b"duplicate", &head)
                    .await;
                let current = self.repository.head().await?;
                if self.repository.read(&path, &current).await?.as_deref() != Some(b"updated") {
                    return Err(Error::new(
                        ErrorCode::Unsupported,
                        "OpenCSG must preserve files when creation is repeated",
                    ));
                }
                match duplicate {
                    Err(error) if error.code == ErrorCode::AlreadyExists => return Ok(()),
                    Err(error) if error.code == ErrorCode::Conflict => {
                        if current == head {
                            return Ok(());
                        }
                    }
                    Err(error) => return Err(error),
                    Ok(()) => {
                        return Err(Error::new(
                            ErrorCode::Unsupported,
                            "OpenCSG did not report repeated creation as rejected",
                        ));
                    }
                }
                tokio::time::sleep(Duration::from_millis(20 * (attempt + 1))).await;
            }
            Err(Error::new(
                ErrorCode::RemoteUnknown,
                "OpenCSG connection check creation remains unconfirmed",
            )
            .retryable())
        }
        .await;
        self.repository.remove_probe(&create_path).await;
        self.repository.remove_probe(&path).await;
        result
    }

    async fn update_probe(&self, path: &str) -> Result<String> {
        for attempt in 0..8 {
            let head = self.repository.head().await?;
            let written = self
                .repository
                .write(Method::PUT, path, b"updated", &head)
                .await;
            let current = self.repository.head().await?;
            if self.repository.read(path, &current).await?.as_deref() == Some(b"updated") {
                return Ok(head);
            }
            if let Err(error) = written
                && !error.retryable
            {
                return Err(error);
            }
            tokio::time::sleep(Duration::from_millis(20 * (attempt + 1))).await;
        }
        Err(Error::new(
            ErrorCode::RemoteUnknown,
            "OpenCSG connection check update remains unconfirmed",
        )
        .retryable())
    }

    /// Create immutable data, recovering acknowledgements by reading the stored bytes.
    /// An unrelated commit may move the shared Git branch between requests; retry from its new head.
    async fn create(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let _write = self.writes.lock().await;
        for attempt in 0..8 {
            let head = self.repository.head().await?;
            if let Some(existing) = self.repository.read(path, &head).await? {
                return if existing == bytes {
                    Ok(())
                } else {
                    Err(Error::integrity(
                        "OpenCSG immutable file contains different data",
                    ))
                };
            }
            let written = self
                .repository
                .write(Method::POST, path, bytes, &head)
                .await;
            let current = self.repository.head().await?;
            if let Some(existing) = self.repository.read(path, &current).await? {
                return if existing == bytes {
                    Ok(())
                } else {
                    Err(Error::integrity(
                        "OpenCSG immutable file contains different data",
                    ))
                };
            }
            if let Err(error) = written
                && !error.retryable
            {
                return Err(error);
            }
            tokio::time::sleep(Duration::from_millis(20 * (attempt + 1))).await;
        }
        Err(Error::new(
            ErrorCode::RemoteUnknown,
            "OpenCSG file creation remains unconfirmed",
        )
        .retryable())
    }
}
