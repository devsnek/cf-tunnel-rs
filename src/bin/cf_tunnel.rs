#[tokio::main]
async fn main() {
    let tunnel = cf_tunnel::Tunnel::new().await.unwrap();
    tunnel.test().await.unwrap();
}
