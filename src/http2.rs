use crate::{rpc::RpcClient, util::Join, Error};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use bytes::Bytes;
use futures::TryStreamExt;
use h2::{server::SendResponse, RecvStream, SendStream};
use http_body_util::{BodyStream, StreamBody};
use hyper::{body::Frame, Request, Response};
use hyper_util::rt::TokioIo;
use std::{
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{duplex, DuplexStream},
    net::TcpStream,
};
use tokio_rustls::{
    rustls::{pki_types::ServerName, ClientConfig, KeyLogFile, RootCertStore},
    TlsConnector,
};
use tokio_util::io::StreamReader;

fn create_tls_client_config() -> Result<ClientConfig, Error> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs()? {
        roots.add(cert).unwrap();
    }
    let mut x = std::io::BufReader::new(include_str!("../cf_root.pem").as_bytes());
    for x in rustls_pemfile::read_all(&mut x).flatten() {
        if let rustls_pemfile::Item::X509Certificate(cert) = x {
            roots.add(cert).unwrap();
        }
    }

    let mut client_config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    client_config.alpn_protocols = vec![b"h2".into()];
    client_config.key_log = Arc::new(KeyLogFile::new());

    Ok(client_config)
}

#[derive(Debug)]
pub struct Http2 {
    rx: tokio::sync::mpsc::Receiver<Result<(Request<RecvStream>, SendResponse<Bytes>), h2::Error>>,
    rpc: RpcClient,
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

        let rpc = {
            let (request, mut send_response) = h2.accept().await.unwrap()?;
            let (_head, body) = request.into_parts();
            let response = Response::builder()
                .status(200)
                .header("content-type", "application/octet-stream")
                .body(())
                .unwrap();
            let send_stream = send_response.send_response(response, false)?;

            RpcClient::new(
                BodyWriter(send_stream),
                StreamReader::new(RecvBodyStream(body)),
            )
        };

        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::task::spawn(async move {
            while let Some(request) = h2.accept().await {
                let _ = tx.send(request).await;
            }
        });

        Ok(Self { rx, rpc })
    }
}

#[async_trait::async_trait]
impl crate::ProtocolImpl for Http2 {
    fn rpc(&self) -> &RpcClient {
        &self.rpc
    }

    async fn accept(&mut self) -> Result<Option<DuplexStream>, Error> {
        match self.rx.recv().await {
            Some(request) => {
                let (virt, other) = duplex(4096);

                tokio::task::spawn(async move {
                    handle_request(request, other).await.unwrap();
                });

                Ok(Some(virt))
            }
            None => Ok(None),
        }
    }
}

async fn handle_request(
    request: Result<(Request<RecvStream>, SendResponse<Bytes>), h2::Error>,
    mut other_virt: DuplexStream,
) -> Result<(), Error> {
    let (request, mut send_response) = request?;
    match request
        .headers()
        .get("cf-cloudflared-proxy-connection-upgrade")
        .and_then(|h| h.to_str().ok())
    {
        Some("control-stream") => {
            unreachable!("control");
        }
        Some("update-configuration") => {
            unimplemented!("config");
        }
        Some("websocket") => {
            unimplemented!("websocket");
        }
        _ => {
            if request.headers().contains_key("cf-cloudflared-proxy-src") {
                let (_, body) = request.into_parts();
                let response = Response::builder()
                    .status(200)
                    .header("cf-cloudflared-response-meta", r#"{"src": "origin"}"#)
                    .body(())
                    .unwrap();

                let send_stream = send_response.send_response(response, false)?;

                let body_reader = StreamReader::new(RecvBodyStream(body));
                let body_writer = BodyWriter(send_stream);

                let mut join = Join::new(body_writer, body_reader);

                tokio::io::copy_bidirectional(&mut other_virt, &mut join).await?;
                join.split().0 .0.send_data(Bytes::new(), true)?;
            } else {
                let (head, body) = request.into_parts();

                let body = StreamBody::new(RecvBodyStream(body).map_ok(Frame::data));
                let mut request = Request::builder()
                    .method(head.method)
                    .uri(
                        head.uri
                            .path_and_query()
                            .map(|p| p.to_string())
                            .unwrap_or("/".into()),
                    )
                    .header("host", head.uri.host().unwrap())
                    .body(body)?;
                request.headers_mut().extend(head.headers);

                let (mut send_request, connection) =
                    hyper::client::conn::http1::handshake(TokioIo::new(other_virt)).await?;
                tokio::task::spawn(async move {
                    connection.with_upgrades().await.unwrap();
                });
                send_request.ready().await?;
                let response = send_request.send_request(request).await?;

                let (head, body) = response.into_parts();

                let mut response = Response::builder()
                    .status(head.status)
                    .header("cf-cloudflared-response-meta", r#"{"src": "origin"}"#)
                    .body(())
                    .unwrap();

                let mut user_headers = Vec::new();
                for (k, v) in head.headers.iter() {
                    if k == "content-type" || k == "content-length" {
                        response.headers_mut().insert(k, v.to_owned());
                    }
                    let k = STANDARD_NO_PAD.encode(k);
                    let v = STANDARD_NO_PAD.encode(v);
                    user_headers.push(format!("{}:{}", k, v));
                }
                response.headers_mut().insert(
                    "cf-cloudflared-response-headers",
                    user_headers.join(";").try_into()?,
                );

                let send_stream = send_response.send_response(response, false)?;
                let mut body_reader = StreamReader::new(
                    BodyStream::new(body)
                        .map_ok(|f| f.into_data().unwrap())
                        .map_err(std::io::Error::other),
                );
                let mut body_writer = BodyWriter(send_stream);

                tokio::io::copy(&mut body_reader, &mut body_writer).await?;

                body_writer.0.send_data(Bytes::new(), true)?;
            }
        }
    }
    Ok(())
}

struct BodyWriter(SendStream<Bytes>);

impl tokio::io::AsyncWrite for BodyWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        self.0.reserve_capacity(buf.len());
        std::task::ready!(self.0.poll_capacity(cx))
            .transpose()
            .map_err(std::io::Error::other)?;
        let size = std::cmp::min(buf.len(), self.0.capacity());
        let buf = Bytes::copy_from_slice(&buf[0..size]);
        self.0
            .send_data(buf, false)
            .map_err(std::io::Error::other)?;
        Poll::Ready(Ok(size))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(
            self.0
                .send_data(Bytes::new(), true)
                .map_err(std::io::Error::other),
        )
    }
}

struct RecvBodyStream(RecvStream);

impl futures::Stream for RecvBodyStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let r = std::task::ready!(self.0.poll_data(cx));
        Poll::Ready(match r {
            Some(Ok(v)) => Some(Ok(v)),
            Some(Err(e)) => Some(Err(std::io::Error::other(e))),
            None => None,
        })
    }
}
