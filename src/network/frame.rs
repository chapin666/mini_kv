use std::io::{Read, Write};
use crate::{CommandRequest, CommandResponse, KvError};
use bytes::{Buf, BufMut, BytesMut};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use tokio::io::{AsyncRead, AsyncReadExt};
use prost::Message;
use tracing::debug;

/// 长度占用 4 字节
pub const LEN_LEN: usize = 4;

/// 长度占 31 bit，所以最大 frame 是 2G
const MAX_FRAME: usize = 2 * 1024 * 1024 * 1024;

/// 如果 payload 超过了 1436 字节，就做压缩
const COMPRESSION_LIMIT: usize = 1436;

/// 代表压缩的 bit （整个长度 4 字节的最高位）
const COMPRESSION_BIT: usize = 1 << 31;

pub trait FrameCoder
    where
        Self: Message + Sized + Default,
{
    // 把一个 Message encode 变成一个 frame
    fn encode_frame(&self, buf: &mut BytesMut) -> Result<(), KvError> {
        let size = self.encoded_len();

        if size >= MAX_FRAME {
            return Err(KvError::FrameError);
        }

        // 我们先写入长度，如果需要压缩，再重写压缩后的长度
        buf.put_u32(size as _);

        if size > COMPRESSION_LIMIT {
            let mut buf1 = Vec::with_capacity(size);
            self.encode(&mut buf1)?;

            // BytesMut支持逻辑上的 split （之后还能unsplit）
            // 所以我们先把长度这 4 个字节拿走，清除
            let payload = buf.split_off(LEN_LEN);
            buf.clear();

            // 处理 gzip 压缩，具体可以参考 flate2 文档
            let mut encoder = GzEncoder::new(payload.writer(), Compression::default());
            encoder.write_all(&buf1[..])?;


            // 压缩完成后，从 gzip encoder 中把 BytesMut 再拿回来
            let payload = encoder.finish()?.into_inner();
            debug!("Encode a frame: size {}({})", size, payload.len());

            // 写入压缩后的长度
            buf.put_u32((payload.len() | COMPRESSION_BIT) as _);

            // 把 BytesMut 再合并回来
            buf.unsplit(payload);

            Ok(())
        } else {
            self.encode(buf)?;
            Ok(())
        }
    }

    /// 把一个完整的 frame decode 成一个 Message
    fn decode_frame(buf: &mut BytesMut) -> Result<Self, KvError> {
        let header = buf.get_u32() as usize;
        let (len, compressed) = decode_header(header);
        debug!("Got a frame: msg len {}, compressed {}", len, compressed);

        if compressed {
            let mut decoder = GzDecoder::new(&buf[..len]);
            let mut buf1 = Vec::with_capacity(len * 2);
            decoder.read_to_end(&mut buf1)?;
            buf.advance(len);

            // decode 成相应的信息
            Ok(Self::decode(&buf1[..buf1.len()])?)
        } else {
            let msg = Self::decode(&buf[..len])?;
            buf.advance(len);
            Ok(msg)
        }
    }
}

impl FrameCoder for CommandRequest {}

impl FrameCoder for CommandResponse {}

fn decode_header(header: usize) -> (usize, bool) {
    let len = header & !COMPRESSION_BIT;
    let compressed = header & COMPRESSION_BIT == COMPRESSION_BIT;
    (len, compressed)
}

/// 从 stream 中读取一个完整的 frame
pub async fn read_frame<S>(stream: &mut S, buf: &mut BytesMut) -> Result<(), KvError>
    where S: AsyncRead + Unpin + Send,
{
    let header = stream.read_u32().await? as usize;
    let (len, _compressed) = decode_header(header);

    // 如果没有这么大的内存，就至少分配一个 frame 的内存，保存它可用
    buf.reserve(LEN_LEN + len);
    buf.put_u32(header as _);

    // advance_mut 是 unsafe 的原因是，当前位置 pos 到 pos + len,
    // 这段内存目前没有初始化，就是为了 reserve 这段内存，然后从 stream
    // 里读取，读取完，它既是初始化的。所以，我们这么用是安全的
    unsafe {
        buf.advance_mut(len)
    }
    stream.read_exact(&mut buf[LEN_LEN..]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use super::*;
    use crate::Value;
    use bytes::Bytes;
    use tokio::io::ReadBuf;
    use crate::utils::DummyStream;

    #[test]
    fn command_request_encode_decode_should_work() {
        let mut buf = BytesMut::new();

        let cmd = CommandRequest::new_hset("t1", "k1", "v1".into());
        cmd.encode_frame(&mut buf).unwrap();

        assert_eq!(is_compressed(&buf), false);

        let cmd1 = CommandRequest::decode_frame(&mut buf).unwrap();
        assert_eq!(cmd, cmd1);
    }

    #[test]
    fn command_response_encode_decode_should_work() {
        let mut buf = BytesMut::new();

        let values: Vec<Value> = vec![1.into(), "hello".into(), b"data".into()];
        let res: CommandResponse = values.into();
        res.encode_frame(&mut buf).unwrap();

        assert_eq!(is_compressed(&buf), false);

        let res1 = CommandResponse::decode_frame(&mut buf).unwrap();
        assert_eq!(res, res1);
    }

    #[test]
    fn command_response_compressed_encode_decode_should_work() {
        let mut buf = BytesMut::new();

        let value: Value = Bytes::from(vec![0u8; COMPRESSION_LIMIT + 1]).into();
        let res: CommandResponse = value.into();
        res.encode_frame(&mut buf).unwrap();

        assert_eq!(is_compressed(&buf), true);

        let res1 = CommandResponse::decode_frame(&mut buf).unwrap();
        assert_eq!(res, res1);
    }

    #[tokio::test]
    async fn read_frame_should_work() {
        let mut buf = BytesMut::new();
        let cmd = CommandRequest::new_hget("t1", "k1");
        cmd.encode_frame(&mut buf).unwrap();

        let mut stream = DummyStream{ buf };
        let mut data = BytesMut::new();
        read_frame(&mut stream, &mut data).await.unwrap();

        let cmd1 = CommandRequest::decode_frame(&mut data).unwrap();
        assert_eq!(cmd, cmd1);
    }

    fn is_compressed(data: &[u8]) -> bool {
        if let &[v] = &data[..1] {
            v >> 7 == 1
        } else {
            false
        }
    }
}