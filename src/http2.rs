use crate::{rpc::RpcClient, Error, HttpBody, HttpService};
use bytes::Bytes;
use futures::TryStreamExt;
use h2::{
    server::{Connection, SendResponse},
    RecvStream, SendStream,
};
use http_body_util::{BodyStream, StreamBody};
use hyper::{body::Frame, Request, Response};
use std::{
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::net::TcpStream;
use tokio_rustls::{
    client::TlsStream,
    rustls::{pki_types::ServerName, ClientConfig, RootCertStore},
    TlsConnector,
};
use tokio_util::io::StreamReader;

fn create_tls_client_config() -> Result<ClientConfig, Error> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs()? {
        roots.add(cert).unwrap();
    }
    let mut x = std::io::BufReader::new(include_str!("./cf_root.pem").as_bytes());
    for x in rustls_pemfile::read_all(&mut x).flatten() {
        if let rustls_pemfile::Item::X509Certificate(cert) = x {
            roots.add(cert).unwrap();
        }
    }

    let mut client_config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    client_config.alpn_protocols = vec![b"h2".into()];

    Ok(client_config)
}

#[derive(Debug)]
pub struct Http2 {
    h2: Connection<TlsStream<TcpStream>, Bytes>,
    rpc: RpcClient,
    inner: Http2Inner,
}

impl Http2 {
    pub async fn connect(remote: SocketAddr) -> Result<Self, Error> {
        let server_name = ServerName::try_from("h2.cftunnel.com").unwrap();
        let tcp = TcpStream::connect(&remote).await?;

        let client_crypto = create_tls_client_config()?;

        let connector = TlsConnector::from(Arc::new(client_crypto));
        let tls = connector.connect(server_name, tcp).await?;

        let mut h2 = h2::server::Builder::new()
            .enable_connect_protocol()
            .handshake(tls)
            .await?;

        let inner = Http2Inner {};

        let rpc = {
            let (request, mut send_response) = h2.accept().await.unwrap()?;
            let (_head, body) = request.into_parts();

            let send_stream = send_response
                .send_response(Response::builder().status(200).body(()).unwrap(), false)?;

            RpcClient::new((
                BodyWriter(send_stream),
                StreamReader::new(RecvBodyStream(body)),
            ))
        };

        Ok(Self { h2, rpc, inner })
    }
}

#[async_trait::async_trait]
impl crate::ProtocolImpl for Http2 {
    fn rpc(&self) -> &RpcClient {
        &self.rpc
    }

    async fn serve(&mut self, service: HttpService) -> Result<(), Error> {
        while let Some(request) = self.h2.accept().await {
            let inner = self.inner.clone();
            let service = service.clone();
            tokio::spawn(async move {
                inner.handle_request(request, service).await.unwrap();
            });
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
struct Http2Inner {}

impl Http2Inner {
    async fn handle_request<S>(
        &self,
        request: Result<(Request<RecvStream>, SendResponse<Bytes>), h2::Error>,
        mut service: S,
    ) -> Result<(), Error>
    where
        S: tower::Service<Request<HttpBody>> + Send,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        S::Response: Into<Response<HttpBody>>,
    {
        let (request, mut send_response) = request?;
        match request
            .headers()
            .get("cf-cloudflared-proxy-connection-upgrade")
            .and_then(|h| h.to_str().ok())
        {
            Some("control-stream") => {
                unreachable!();
            }
            _ => {
                let (head, body) = request.into_parts();
                let body = HttpBody::new(StreamBody::new(RecvBodyStream(body).map_ok(Frame::data)));
                let request = Request::from_parts(head, body);

                let response = service.call(request).await.map_err(Into::into).unwrap();

                let (head, body) = response.into().into_parts();
                let response = Response::from_parts(head, ());

                let mut body_reader =
                    StreamReader::new(BodyStream::new(body).map_ok(|f| f.into_data().unwrap()));
                let mut body_writer = BodyWriter(send_response.send_response(response, false)?);

                tokio::io::copy(&mut body_reader, &mut body_writer).await?;

                body_writer.0.send_reset(h2::Reason::NO_ERROR);
            }
        }
        Ok(())
    }
}

struct BodyWriter(SendStream<Bytes>);

impl tokio::io::AsyncWrite for BodyWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        loop {
            if self.0.capacity() < buf.len() {
                self.0.reserve_capacity(buf.len());
                match self.0.poll_capacity(cx) {
                    Poll::Ready(Some(Ok(_))) => {
                        continue;
                    }
                    Poll::Ready(Some(Err(e))) => {
                        return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, e)));
                    }
                    Poll::Ready(None) => unreachable!(),
                    Poll::Pending => {
                        return Poll::Pending;
                    }
                }
            }
            break Poll::Ready(match self.0.send_data(buf.to_vec().into(), false) {
                Ok(()) => Ok(buf.len()),
                Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
            });
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }
}

struct RecvBodyStream(RecvStream);

impl futures::Stream for RecvBodyStream {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Poll::Ready(r) = self.0.poll_data(cx) else {
            return Poll::Pending;
        };
        Poll::Ready(match r {
            Some(Ok(v)) => Some(Ok(v)),
            Some(Err(e)) => Some(Err(std::io::Error::new(std::io::ErrorKind::Other, e))),
            None => None,
        })
    }
}