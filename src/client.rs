use std::time::Duration;
use anyhow::Result;
use futures::StreamExt;
use tokio::net::TcpStream;
use tokio::time;
use tokio_util::compat::Compat;
use tracing::info;
use yamux::Stream;
use mini_kv::{CommandRequest, KvError, ProstClientStream, TlsClientConnector, YamuxCtrl};


#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // 以后用配置替换
    let ca_cert = include_str!("../fixtures/ca.cert");
    let addr = "127.0.0.1:9527";

    let connector = TlsClientConnector::new("kvserver.acme.inc", None, Some(ca_cert))?;
    let stream = TcpStream::connect(addr).await?;
    let stream = connector.connect(stream).await?;

    let mut ctrl = YamuxCtrl::new_client(stream, None);

    let channel = "lobby";
    start_publishing(ctrl.open_stream().await?, channel)?;


    let mut stream = ctrl.open_stream().await?;

    // 生成一个 HSET 命令
    let cmd = CommandRequest::new_hset("table1", "hello", "world".to_string().into());
    let data = stream.execute_unary(&cmd).await?;
    info!("Got response {:?}", data);

    // 生成一个 Subscribe 命令
    let cmd = CommandRequest::new_subscribe(channel);
    let mut stream = stream.execute_streaming(&cmd).await?;

    // 取消订阅
    let id = stream.id;
    start_unsubscribe(ctrl.open_stream().await?, channel, id)?;

    while let Some(Ok(data)) = stream.next().await {
        println!("Got published data: {:?}", data);
    }

    println!("Done!");

    Ok(())
}

fn start_publishing(
    mut stream: ProstClientStream<Compat<yamux::Stream>>,
    name: &str,
) -> Result<(), KvError> {
    let cmd = CommandRequest::new_publish(name, vec![1.into(), 2.into(), "hello".into()]);
    tokio::spawn(async move {
        time::sleep(Duration::from_millis(1000)).await;
        let res = stream.execute_unary(&cmd).await.unwrap();
        println!("Finished publishing: {:?}", res);
    });

    Ok(())
}

fn start_unsubscribe(
    mut stream: ProstClientStream<Compat<yamux::Stream>>,
    name: &str,
    id: u32,
) -> Result<(), KvError> {
    let cmd = CommandRequest::new_unsubscribe(name, id as _);
    tokio::spawn(async move {
        time::sleep(Duration::from_millis(2000)).await;
        let res = stream.execute_unary(&cmd).await.unwrap();
        println!("Finished unsubscribing: {:?}", res);
    });

    Ok(())
}