use bytes::BytesMut;
use futures::{ready, FutureExt, Sink, Stream};
use std::{
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{read_frame, FrameCoder, KvError};

/// 处理 KV server prost frame 的 stream
pub struct ProstStream<S, In, Out> {
    // inner stream
    stream: S,

    // 写缓存
    wbuf: BytesMut,
    // 写入了多少个字节
    written: usize,
    // 读缓存
    rbuf: BytesMut,

    // 类型占位符
    _in: PhantomData<In>,
    _out: PhantomData<Out>,
}

impl<S, In, Out> Stream for ProstStream<S, In, Out> where
    S: AsyncRead + AsyncWrite + Unpin + Send,
    In: Unpin + Send + FrameCoder,
    Out: Unpin + Send,
{
    type Item = Result<In, KvError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // 上一次调用结束后 rbuf 应该为空
        assert!(self.rbuf.len() == 0);

        // 从 rbuf 中分离出 rest
        let mut rest = self.rbuf.split_off(0);

        // 使用 read_frame 来获取数据
        let fut = read_frame(&mut self.stream, &mut rest);
        ready!(Box::pin(fut).poll_unpin(cx))?;

        self.rbuf.unsplit(rest);

        Poll::Ready(Some(In::decode_frame(&mut self.rbuf)))
    }
}

impl<S, In, Out> Sink<&Out> for ProstStream<S, In, Out> where
    S: AsyncRead + AsyncWrite + Unpin,
    In: Unpin + Send,
    Out: Unpin + Send + FrameCoder,
{
    type Error = KvError;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: &Out) -> Result<(), Self::Error> {
        let this = self.get_mut();
        item.encode_frame(&mut this.wbuf)?;
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();

        while this.written != this.wbuf.len() {
            let n = ready!(Pin::new(&mut this.stream).poll_write(cx, &this.wbuf[this.written..]))?;
            this.written += n;
        }

        // 清除 wbuf
        this.wbuf.clear();
        this.written = 0;

        // 调用 stream 的 poll_flush 确保写入
        ready!(Pin::new(&mut this.stream).poll_flush(cx)?);
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll_flush(cx))?;
        ready!(Pin::new(&mut self.stream).poll_shutdown(cx))?;
        Poll::Ready(Ok(()))
    }
}

impl<S, In, Out> Unpin for ProstStream<S, In, Out> where S: Unpin {}


impl<S, In, Out> ProstStream<S, In, Out> where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            written: 0,
            wbuf: BytesMut::new(),
            rbuf: BytesMut::new(),
            _in: PhantomData::default(),
            _out: PhantomData::default(),
        }
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use bytes::BytesMut;
    use futures::{SinkExt, StreamExt};
    use crate::{CommandRequest, utils::DummyStream};

    #[allow(clippy::all)]
    #[tokio::test]
    async fn prost_stream_should_work() -> Result<()> {
        let buf = BytesMut::new();
        let stream = DummyStream { buf };

        // 创建 ProstStream
        let mut stream = ProstStream::<_, CommandRequest, CommandRequest>::new(stream);
        let cmd = CommandRequest::new_hget("t1", "k1");

        // 使用 ProstStream 发送数据
        stream.send(&cmd).await?;

        // 使用 ProstStream 接收数据
        if let Some(Ok(s)) = stream.next().await {
            assert_eq!(s, cmd);
        } else {
            assert!(false);
        }
        Ok(())
    }
}