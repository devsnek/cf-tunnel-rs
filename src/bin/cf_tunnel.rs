use clap::{Parser, Subcommand};
use http::uri::{Authority, Scheme};
use reqwest::Client;

#[derive(Parser, Debug)]
#[command(version, propagate_version = true)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone)]
struct ProxyUrl(Scheme, Authority);

impl std::str::FromStr for ProxyUrl {
    type Err = http::uri::InvalidUri;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<_> = s.split("://").collect();
        let scheme = parts[0].parse()?;
        let authority = parts[1].parse()?;
        Ok(Self(scheme, authority))
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    Try {
        url: ProxyUrl,
        #[arg(short, long)]
        protocol: Option<cf_tunnel::Protocol>,
    },
}

#[derive(Debug, Clone)]
struct ProxyService {
    client: Client,
    url: ProxyUrl,
}

impl tower::Service<http::Request<cf_tunnel::HttpBody>> for ProxyService {
    type Response = http::Response<cf_tunnel::HttpBody>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: hyper::Request<cf_tunnel::HttpBody>) -> Self::Future {
        let url = self.url.clone();
        let client = self.client.clone();
        let f = async move {
            use http_body_util::BodyExt;

            let (head, body) = req.into_parts();

            let mut uri = head.uri.into_parts();
            uri.scheme = Some(url.0);
            uri.authority = Some(url.1);
            let uri = http::uri::Uri::from_parts(uri).unwrap();

            // mess because reqwest is on http 0.2 and hyper is on http 1.0

            let resw = client
                .request(
                    reqwest::Method::from_bytes(head.method.as_str().as_bytes()).unwrap(),
                    uri.to_string(),
                )
                .headers(
                    head.headers
                        .iter()
                        .map(|(k, v)| {
                            (
                                reqwest::header::HeaderName::from_bytes(k.as_ref()).unwrap(),
                                reqwest::header::HeaderValue::from_bytes(v.as_ref()).unwrap(),
                            )
                        })
                        .collect(),
                )
                .body(body.collect().await.unwrap().to_bytes())
                .send()
                .await
                .unwrap();

            let mut res = hyper::Response::builder()
                .status(hyper::StatusCode::from_u16(resw.status().as_u16()).unwrap());

            for (k, v) in resw.headers() {
                res = res.header(
                    hyper::header::HeaderName::from_bytes(k.as_ref()).unwrap(),
                    hyper::header::HeaderValue::from_bytes(v.as_ref()).unwrap(),
                );
            }

            use futures::StreamExt;
            let stream = resw.bytes_stream().map(|result| match result {
                Ok(v) => Ok(hyper::body::Frame::data(v)),
                Err(e) => Err(std::io::Error::other(e)),
            });
            let res = res
                .body(cf_tunnel::HttpBody::new(http_body_util::StreamBody::new(
                    stream,
                )))
                .unwrap();

            Ok(res)
        };

        Box::pin(f)
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    match args.command {
        Command::Try { url, protocol } => {
            let try_tunnel = cf_tunnel::try_tunnel::create_try_tunnel().await.unwrap();

            let tunnel = cf_tunnel::Tunnel::new().await.unwrap();

            println!("https://{}", try_tunnel.hostname);

            let service = ProxyService {
                client: Client::new(),
                url: url.to_owned(),
            };

            tunnel
                .serve(&try_tunnel, cf_tunnel::HttpService::new(service), protocol)
                .await
                .unwrap();
        }
    }
}
