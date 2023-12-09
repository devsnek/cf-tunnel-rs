#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    ResolveError(#[from] hickory_resolver::error::ResolveError),
    #[error("{0}")]
    Rustls(#[from] rustls::Error),
    #[error("{0}")]
    StdIo(#[from] std::io::Error),
    #[error("{0}")]
    ConnectError(#[from] quinn::ConnectError),
    #[error("{0}")]
    ConnectionError(#[from] quinn::ConnectionError),
    #[error("{0}")]
    ReadExactError(#[from] quinn::ReadExactError),
    #[error("{0}")]
    Capnp(#[from] capnp::Error),
    #[error("{0}")]
    NotInSchema(#[from] capnp::NotInSchema),
    #[error("{0}")]
    Utf8Error(#[from] std::str::Utf8Error),
    #[error("{0}")]
    RecvError(#[from] tokio::sync::oneshot::error::RecvError),
    #[error("{0}")]
    ReadError(#[from] quinn::ReadError),
    #[error("{0}")]
    WriteError(#[from] quinn::WriteError),
    #[error("{0}")]
    InvalidMethod(#[from] http::method::InvalidMethod),
    #[error("{0}")]
    HttpError(#[from] http::Error),
    #[error("{0}")]
    ParseIntError(#[from] std::num::ParseIntError),
    #[error("{0}")]
    ToStrError(#[from] http::header::ToStrError),
    #[error("{0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("{0}")]
    Uuid(#[from] uuid::Error),
    #[error("{0}")]
    DecodeError(#[from] base64::DecodeError),
    #[error("RPC Error: {0}")]
    RpcError(String),
}
