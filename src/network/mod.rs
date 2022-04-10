mod frame;
mod tls;
mod stream;
mod multiplex;
mod stream_result;

pub use frame::{read_frame, FrameCoder};
pub use tls::{TlsClientConnector, TlsServerAcceptor};
pub use multiplex::YamuxCtrl;

use bytes::BytesMut;
use futures::{SinkExt, StreamExt};
use prost::encoding::group::encode;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tracing::info;

use crate::{CommandRequest, CommandResponse, KvError, Service, Storage};
use crate::network::stream::ProstStream;
use crate::network::stream_result::StreamResult;

/// 处理服务器端的某个 accept 下来的 socket 的读写
pub struct ProstServerStream<S, Store> {
    inner: ProstStream<S, CommandRequest, CommandResponse>,
    service: Service<Store>,
}

/// 处理客户端的 socket 读写
pub struct ProstClientStream<S> {
    inner: ProstStream<S, CommandResponse, CommandRequest>,
}

impl<S, Store> ProstServerStream<S, Store> where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    Store: Storage,
{
    pub fn new(stream: S, service: Service<Store>) -> Self {
        Self {
            inner: ProstStream::new(stream),
            service,
        }
    }

    pub async fn process(mut self) -> Result<(), KvError> {
        let stream = &mut self.inner;
        while let Some(Ok(cmd)) = stream.next().await {
            info!("Got a new command: {:?}", cmd);
            let mut res = self.service.execute(cmd);
            while let Some(data) = res.next().await {
                stream.send(&data).await.unwrap();
            }
        }
        // info!("Client {:?} disconnected", self.addr);
        Ok(())
    }
}

impl<S> ProstClientStream<S> where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub fn new(stream: S) -> Self {
        Self {
            inner: ProstStream::new(stream),
        }
    }

    pub async fn execute_unary(&mut self, cmd: &CommandRequest) -> Result<CommandResponse, KvError> {
        let stream = &mut self.inner;
        stream.send(cmd).await?;

        match stream.next().await {
            Some(v) => v,
            None => Err(KvError::Internal("Didn't get any response".into())),
        }
    }

    pub async fn execute_streaming(self, cmd: &CommandRequest) -> Result<StreamResult, KvError> {
        let mut stream = self.inner;
        stream.send(cmd).await?;
        stream.close().await?;
        StreamResult::new(stream).await
    }
}

#[cfg(test)]
mod test {
    use anyhow::Result;
    use std::net::SocketAddr;
    use bytes::Bytes;
    use tokio::net::{TcpListener, TcpStream};
    use crate::{assert_res_ok, MemTable, ServiceInner, Value};
    use super::*;

    #[tokio::test]
    async fn client_server_basic_communication_should_work() -> anyhow::Result<()> {
        let addr = start_server().await?;

        let stream = TcpStream::connect(addr).await?;
        let mut client = ProstClientStream::new(stream);

        // 发送 hset, 等待回应
        let cmd = CommandRequest::new_hset("t1", "k1", "v1".into());
        let res = client.execute_unary(&cmd).await.unwrap();

        // 第一次 HSET 服务器应该返回 NONE
        assert_res_ok(&res, &[Value::default()], &[]);

        // 再发一个 HSET
        let cmd = CommandRequest::new_hget("t1", "k1");
        let res = client.execute_unary(&cmd).await?;

        assert_res_ok(&res, &["v1".into()], &[]);

        Ok(())
    }

    async fn client_server_compression_should_work() -> anyhow::Result<()> {
        let addr = start_server().await?;

        let stream = TcpStream::connect(addr).await?;
        let mut client = ProstClientStream::new(stream);

        let v: Value = Bytes::from(vec![0u8; 16384]).into();
        let cmd = CommandRequest::new_hset("t2", "k2", v.clone().into());
        let res = client.execute_unary(&cmd).await?;
        assert_res_ok(&res, &[Value::default()], &[]);

        let cmd = CommandRequest::new_hget("t2", "k2");
        let res = client.execute_unary(&cmd).await?;
        assert_res_ok(&res, &[v.into()], &[]);

        Ok(())
    }

    async fn start_server() -> Result<SocketAddr> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let service: Service = ServiceInner::new(MemTable::new()).into();
                let server = ProstServerStream::new(stream, service);
                tokio::spawn(server.process());
            }
        });

        Ok(addr)
    }
}

#[cfg(test)]
pub mod utils {
    use std::io::Error;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use bytes::{BufMut, BytesMut};
    use futures::future::ok;
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    pub struct DummyStream {
        pub buf: BytesMut,
    }

    impl AsyncRead for DummyStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let len = buf.capacity();
            let data = self.get_mut().buf.split_to(len);
            buf.put_slice(&data);
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for DummyStream {
        fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
            self.get_mut().buf.put_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }
    }
}
