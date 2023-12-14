#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    ResolveError(#[from] hickory_resolver::error::ResolveError),
    #[error(transparent)]
    Rustls(#[from] rustls::Error),
    #[error(transparent)]
    StdIo(#[from] std::io::Error),
    #[error(transparent)]
    ConnectError(#[from] quinn::ConnectError),
    #[error(transparent)]
    ConnectionError(#[from] quinn::ConnectionError),
    #[error(transparent)]
    ReadExactError(#[from] quinn::ReadExactError),
    #[error(transparent)]
    Capnp(#[from] capnp::Error),
    #[error(transparent)]
    NotInSchema(#[from] capnp::NotInSchema),
    #[error(transparent)]
    Utf8Error(#[from] std::str::Utf8Error),
    #[error(transparent)]
    RecvError(#[from] tokio::sync::oneshot::error::RecvError),
    #[error(transparent)]
    ReadError(#[from] quinn::ReadError),
    #[error(transparent)]
    WriteError(#[from] quinn::WriteError),
    #[error(transparent)]
    ParseIntError(#[from] std::num::ParseIntError),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Uuid(#[from] uuid::Error),
    #[error(transparent)]
    DecodeError(#[from] base64::DecodeError),
    #[error(transparent)]
    InvalidMethod(#[from] hyper::http::method::InvalidMethod),
    #[error(transparent)]
    ToStrError(#[from] hyper::header::ToStrError),
    #[error(transparent)]
    HttpError(#[from] hyper::http::Error),
    #[error(transparent)]
    HyperError(#[from] hyper::Error),
    #[error(transparent)]
    InvalidHeaderValue(#[from] hyper::header::InvalidHeaderValue),
    #[error(transparent)]
    H2Error(#[from] h2::Error),
    #[error(transparent)]
    Cloudflare(#[from] cloudflare::framework::Error),
    #[error(transparent)]
    CfApiFailure(#[from] cloudflare::framework::response::ApiFailure),
    #[error("RPC Error: {0}")]
    RpcError(String),
    #[error("Version mismatch")]
    VersionMismatch,
    #[error("No method")]
    NoMethod,
    #[error("Send error")]
    SendError,
    #[error("Edge discovery found zero addresses")]
    EdgeDiscoveryFailed,
    #[error("Invalid protocol, must be one of quic, http2")]
    InvalidProtocol,
}
