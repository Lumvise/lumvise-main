use std::{io, num::TryFromIntError};

use prost::Message;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame ended before its eight-byte length prefix")]
    TruncatedLength,
    #[error("frame declares {declared} bytes, which cannot fit in local usize")]
    LengthNotRepresentable { declared: u64 },
    #[error("frame ended before its declared {declared}-byte body")]
    TruncatedBody { declared: u64 },
    #[error("invalid protobuf envelope: {0}")]
    Protobuf(#[from] prost::DecodeError),
    #[error("I/O while reading or writing a frame: {0}")]
    Io(#[from] io::Error),
}

pub fn encode_frame<M: Message>(message: &M) -> Vec<u8> {
    let encoded_len = message.encoded_len();
    let mut frame = Vec::with_capacity(8 + encoded_len);
    frame.extend_from_slice(&(encoded_len as u64).to_be_bytes());
    message
        .encode(&mut frame)
        .expect("Vec has exactly enough capacity for Prost");
    frame
}

pub fn decode_frame<M: Message + Default>(frame: &[u8]) -> Result<M, FrameError> {
    if frame.len() < 8 {
        return Err(FrameError::TruncatedLength);
    }
    let declared = u64::from_be_bytes(frame[..8].try_into().expect("prefix is eight bytes"));
    let body_len =
        usize::try_from(declared).map_err(|_| FrameError::LengthNotRepresentable { declared })?;
    if frame.len() - 8 != body_len {
        return Err(FrameError::TruncatedBody { declared });
    }
    Ok(M::decode(&frame[8..])?)
}

pub async fn read_frame<R, M>(reader: &mut R) -> Result<M, FrameError>
where
    R: AsyncRead + Unpin,
    M: Message + Default,
{
    let mut length = [0_u8; 8];
    match reader.read_exact(&mut length).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(FrameError::TruncatedLength);
        }
        Err(error) => return Err(FrameError::Io(error)),
    }
    let declared = u64::from_be_bytes(length);
    let body_len = usize::try_from(declared)
        .map_err(|_: TryFromIntError| FrameError::LengthNotRepresentable { declared })?;
    let mut body = vec![0_u8; body_len];
    match reader.read_exact(&mut body).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(FrameError::TruncatedBody { declared });
        }
        Err(error) => return Err(FrameError::Io(error)),
    }
    Ok(M::decode(body.as_slice())?)
}

pub async fn write_frame<W, M>(writer: &mut W, message: &M) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    M: Message,
{
    writer.write_all(&encode_frame(message)).await?;
    writer.flush().await?;
    Ok(())
}
