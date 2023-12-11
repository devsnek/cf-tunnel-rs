use crate::{rpc::RpcClient, Error, HttpBody};
use async_compat::CompatExt;
use futures::TryFutureExt;
use http_body_util::{combinators::BoxBody, BodyExt, Empty as EmptyBody, Full as FullBody};
use std::{collections::HashMap, net::SocketAddr};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) mod quic_metadata_protocol_capnp {
    #![allow(clippy::all)]
    #![allow(unused)]
    include!(concat!(env!("OUT_DIR"), "/quic_metadata_protocol_capnp.rs"));
}

pub const DATA_SIG: [u8; 6] = [0x0A, 0x36, 0xCD, 0x12, 0xA1, 0x3E];
pub const RPC_SIG: [u8; 6] = [0x52, 0xBB, 0x82, 0x5C, 0xDB, 0x65];
pub const VERSION: [u8; 2] = *b"01";

pub async fn connect_quic(remote: SocketAddr) -> Result<quinn::Connection, Error> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs()? {
        roots.add(&rustls::Certificate((*cert).to_owned()))?;
    }
    let mut x = std::io::BufReader::new(include_str!("./cf_root.pem").as_bytes());
    for x in rustls_pemfile::read_all(&mut x).flatten() {
        if let rustls_pemfile::Item::X509Certificate(cert) = x {
            roots.add(&rustls::Certificate((*cert).to_owned()))?;
        }
    }

    let client_crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(std::sync::Arc::new(client_crypto));
    let mut endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap())?;
    endpoint.set_default_client_config(client_config);

    let conn = endpoint.connect(remote, "quic.cftunnel.com")?.await?;

    Ok(conn)
}

async fn build_http_request(
    uri: String,
    metadata: HashMap<String, String>,
    mut rx: impl AsyncRead + Send + Sync + Unpin,
) -> Result<hyper::Request<BoxBody<bytes::Bytes, std::convert::Infallible>>, Error> {
    let Some(method) = metadata.get("HttpMethod") else {
        return Err(Error::NoMethod);
    };
    let method = hyper::Method::from_bytes(method.as_bytes())?;

    let mut builder = hyper::Request::builder().uri(uri).method(method);

    for (k, v) in &metadata {
        if k.starts_with("HttpHeader") {
            let key = k.split(':').nth(1).unwrap();
            builder = builder.header(key, v);
        }
    }

    let body = if let Some(len) = metadata.get("HttpHeader:Content-Length") {
        let mut body = vec![0; len.parse()?];
        rx.read_exact(&mut body).await?;
        BoxBody::new(FullBody::new(body.into()))
    } else {
        BoxBody::new(EmptyBody::new())
    };

    let req = builder.body(body)?;

    Ok(req)
}

#[derive(Debug)]
pub struct Quic {
    conn: quinn::Connection,
    inner: QuicInner,
    pub rpc: RpcClient,
}

impl Quic {
    pub async fn connect(remote: SocketAddr) -> Result<Self, Error> {
        let conn = connect_quic(remote).await?;

        let control_stream = conn.open_bi().await?;

        let rpc = RpcClient::new(control_stream);

        let inner = QuicInner {};

        Ok(Self { conn, rpc, inner })
    }

    pub async fn serve<S>(&self, service: S) -> Result<(), Error>
    where
        S: tower::Service<hyper::Request<HttpBody>> + Send + Clone + 'static,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        S::Response: Into<hyper::Response<HttpBody>> + Send,
        S::Future: Send,
    {
        loop {
            let (tx, mut rx) = self.conn.accept_bi().await?;

            let mut sig = [0; 6];
            rx.read_exact(&mut sig).await?;

            if sig == DATA_SIG {
                let inner = self.inner.clone();
                let service = service.clone();
                tokio::task::spawn(async move {
                    inner.handle_data_stream(tx, rx, service).await.unwrap();
                });
            } else if sig == RPC_SIG {
                unimplemented!("RPC STREAM!");
            } else {
                // ??
            }
        }
    }
}

#[derive(Debug, Clone)]
struct QuicInner {}

impl QuicInner {
    async fn handle_data_stream<S>(
        &self,
        mut tx: impl AsyncWrite + Send + Sync + Unpin,
        mut rx: impl AsyncRead + Send + Sync + Unpin,
        mut service: S,
    ) -> Result<(), Error>
    where
        S: tower::Service<hyper::Request<HttpBody>> + Send,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        S::Response: Into<hyper::Response<HttpBody>>,
    {
        let mut v = [0; 2];
        rx.read_exact(&mut v).await?;
        if v != VERSION {
            return Err(Error::VersionMismatch);
        }

        let r = capnp_futures::serialize::read_message(&mut rx.compat_mut(), Default::default())
            .await?;
        let r = r.into_typed::<quic_metadata_protocol_capnp::connect_request::Owned>();
        let mtype = r.get()?.get_type()?;
        match mtype {
            quic_metadata_protocol_capnp::ConnectionType::Http
            | quic_metadata_protocol_capnp::ConnectionType::Websocket => {
                let uri = r.get()?.get_dest()?.to_string()?;
                let mut metadata = HashMap::new();
                {
                    let m = r.get()?.get_metadata()?;
                    for i in 0..m.len() {
                        let entry = m.reborrow().get(i);
                        metadata
                            .insert(entry.get_key()?.to_string()?, entry.get_val()?.to_string()?);
                    }
                }
                drop(r);

                let result: Result<_, Box<dyn std::error::Error + Send + Sync>> =
                    build_http_request(uri, metadata, &mut rx)
                        .err_into()
                        .and_then(|req| async {
                            match service.call(req).await {
                                Ok(res) => Ok(res),
                                Err(e) => Err(e.into()),
                            }
                        })
                        .and_then(|res| async {
                            let mut builder = capnp::message::TypedBuilder::<
                                quic_metadata_protocol_capnp::connect_response::Owned,
                            >::new_default();

                            let (mut head, body) = res.into().into_parts();

                            let body = match body.collect().await {
                                Ok(body) => body.to_bytes(),
                                Err(e) => {
                                    return Err(e.into());
                                }
                            };

                            head.headers.insert(
                                "Content-Length",
                                hyper::header::HeaderValue::from_str(
                                    format!("{}", body.len()).as_str(),
                                )
                                .unwrap(),
                            );

                            let root = builder.init_root();
                            let mut m = root.init_metadata((head.headers.len() + 1) as _);

                            let mut entry = m.reborrow().get(0);
                            entry.set_key("HttpStatus".into());
                            entry.set_val(format!("{}", head.status.as_u16()).as_str().into());

                            let mut i = 1;
                            for (k, v) in head.headers.iter() {
                                let mut entry = m.reborrow().get(i);
                                i += 1;
                                entry.set_key(format!("HttpHeader:{}", k.as_str()).as_str().into());
                                entry.set_val(v.to_str()?.into());
                            }

                            Ok((builder, body))
                        })
                        .await;

                tx.write_all(&DATA_SIG).await?;
                tx.write_all(&VERSION).await?;

                match result {
                    Ok((builder, body)) => {
                        capnp_futures::serialize::write_message(
                            &mut tx.compat_mut(),
                            builder.into_inner(),
                        )
                        .await?;
                        if !body.is_empty() {
                            tx.write_all(&body).await?;
                        }
                    }
                    Err(e) => {
                        let mut builder = capnp::message::TypedBuilder::<
                            quic_metadata_protocol_capnp::connect_response::Owned,
                        >::new_default();
                        {
                            let mut root = builder.init_root();
                            root.set_error(format!("{e}")[..].into());
                        }
                        capnp_futures::serialize::write_message(
                            &mut tx.compat_mut(),
                            builder.into_inner(),
                        )
                        .await?;
                    }
                }
            }
            quic_metadata_protocol_capnp::ConnectionType::Tcp => {
                unimplemented!("raw tcp is not yet supported");
            }
        }

        Ok(())
    }
}
