#[tokio::main]
async fn main() {
    let try_tunnel = cf_tunnel::try_tunnel::create_try_tunnel().await.unwrap();

    let tunnel = cf_tunnel::Tunnel::new().await.unwrap();

    println!("https://{}", try_tunnel.hostname);

    tunnel.serve(&try_tunnel).await.unwrap();
}
