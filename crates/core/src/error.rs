use std::path::PathBuf;

/// Errors carried across crate boundaries. Each variant names the thing the user
/// can act on, because these strings end up in front of a human.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_include_the_path() {
        let err = Error::io(
            "a/b.toml",
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        assert!(err.to_string().starts_with("a/b.toml"));
    }
}
