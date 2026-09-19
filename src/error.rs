use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("The embedded GifGun contract manifest is malformed: {0}")]
    Manifest(String),
    #[error("The embedded GifGun contract schema is unavailable: {0}")]
    Schema(String),
    #[error("The private GifGun agent state is unavailable: {0}")]
    State(String),
    #[error("The operating system could not complete the local agent operation: {0}")]
    Io(#[from] std::io::Error),
    #[error("The operating system could not create a private session credential.")]
    Random,
    #[error("The local media transfer failed: {0}")]
    Transfer(String),
    #[error("The local GifGun bridge request failed: {0}")]
    Bridge(String),
    #[error("The GifGun agent command is invalid: {0}")]
    Cli(String),
}

pub type AgentResult<T> = Result<T, AgentError>;
