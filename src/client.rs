
use anyhow::Result;
use tokio::net::TcpStream;
use tracing::info;
use mini_kv::{CommandRequest, ProstClientStream, TlsClientConnector};


#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // 以后用配置替换
    let ca_cert = include_str!("../fixtures/ca.cert");
    let addr = "127.0.0.1:9527";

    let connector = TlsClientConnector::new("kvserver.acme.inc", None, Some(ca_cert))?;
    let stream = TcpStream::connect(addr).await?;
    let stream = connector.connect(stream).await?;

    let mut client = ProstClientStream::new(stream);

    let cmd = CommandRequest::new_hset("table1", "hello", "world".to_string().into());
    let data = client.execute(cmd).await?;
    info!("Got response {:?}", data);

    Ok(())
}