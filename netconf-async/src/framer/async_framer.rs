use crate::error::{NetconfClientError, NetconfClientResult};
use crate::framer::{Framer, NETCONF_1_0_TERMINATOR};
use async_trait::async_trait;
use log::debug;
use memchr::memmem;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Default cap on one reply, in bytes.
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 256 * 1024 * 1024;

/// A chunk header carries at most 10 digits ([RFC6242 §4.2](https://www.rfc-editor.org/rfc/rfc6242.html#section-4.2)).
const MAX_CHUNK_DIGITS: usize = 10;

const READ_CHUNK: usize = 8 * 1024;

/// Async 1.0 / 1.1 message framer ([RFC6242 §4](https://www.rfc-editor.org/rfc/rfc6242.html#section-4)).
pub struct AsyncFramer<T> {
    read_buffer: Vec<u8>,
    upgraded: bool,
    max_message_size: usize,

    channel: T,
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncFramer<T> {
    /// Frame `channel` starting in 1.0 mode (`]]>]]>`).
    pub fn new(channel: T) -> Self {
        AsyncFramer {
            read_buffer: Vec::new(),
            upgraded: false,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            channel,
        }
    }

    /// Cap a single reply at `limit` bytes; defaults to [`DEFAULT_MAX_MESSAGE_SIZE`].
    ///
    /// Replies past the limit fail with [`NetconfClientError::MessageTooLarge`]
    /// instead of growing the read buffer without bound.
    pub fn set_max_message_size(&mut self, limit: usize) {
        self.max_message_size = limit;
    }

    /// Borrow the underlying channel, e.g. to shut it down.
    pub fn channel_mut(&mut self) -> &mut T {
        &mut self.channel
    }

    fn too_large(&self) -> NetconfClientError {
        NetconfClientError::MessageTooLarge {
            limit: self.max_message_size,
        }
    }

    /// Read a `\n#N\n` chunk header, or `0` for the `\n##\n` end marker.
    async fn read_header(&mut self) -> NetconfClientResult<u32> {
        let mut buffer = [0u8; 2];
        self.channel.read_exact(&mut buffer).await?;
        if buffer[0] != b'\n' {
            return Err(NetconfClientError::MalformedChunk {
                expected: '\n',
                actual: buffer[0].into(),
            });
        }

        if buffer[1] != b'#' {
            return Err(NetconfClientError::MalformedChunk {
                expected: '#',
                actual: buffer[1].into(),
            });
        }

        let mut chunk_size: u32 = 0;
        let mut digits = 0usize;
        loop {
            let mut buffer = [0u8; 1];
            self.channel.read_exact(&mut buffer).await?;
            let last_read = buffer[0];
            // Second `#` of the `\n##\n` end-of-chunks marker.
            if last_read == b'#' && digits == 0 {
                continue;
            }
            if last_read == b'\n' {
                return Ok(chunk_size);
            }
            if !last_read.is_ascii_digit() {
                return Err(NetconfClientError::MalformedChunk {
                    expected: '0',
                    actual: last_read.into(),
                });
            }
            digits += 1;
            if digits > MAX_CHUNK_DIGITS {
                return Err(self.too_large());
            }
            chunk_size = chunk_size
                .checked_mul(10)
                .and_then(|size| size.checked_add(u32::from(last_read - b'0')))
                .ok_or_else(|| self.too_large())?;
        }
    }

    async fn read_chunked(&mut self) -> NetconfClientResult<String> {
        loop {
            let chunk_size = self.read_header().await? as usize;
            if chunk_size == 0 {
                break;
            }
            let filled = self.read_buffer.len();
            if chunk_size > self.max_message_size - filled.min(self.max_message_size) {
                return Err(self.too_large());
            }
            self.read_buffer.resize(filled + chunk_size, 0);
            self.channel
                .read_exact(&mut self.read_buffer[filled..])
                .await?;
        }
        let response = String::from_utf8_lossy(&self.read_buffer)
            .trim_end()
            .to_string();
        self.read_buffer.clear();
        Ok(response)
    }

    async fn read_end_of_message(&mut self) -> NetconfClientResult<String> {
        let terminator = NETCONF_1_0_TERMINATOR.as_bytes();
        let mut buffer = [0u8; READ_CHUNK];
        // Bytes already scanned; a terminator can straddle two reads, so keep
        // the last `terminator.len() - 1` bytes in scope.
        let mut scanned = 0;
        loop {
            if let Some(offset) = memmem::find(&self.read_buffer[scanned..], terminator) {
                let end = scanned + offset;
                let response = String::from_utf8_lossy(&self.read_buffer[..end])
                    .trim_end()
                    .to_string();
                self.read_buffer.drain(..end + terminator.len());
                return Ok(response);
            }
            scanned = self.read_buffer.len().saturating_sub(terminator.len() - 1);
            if self.read_buffer.len() > self.max_message_size {
                return Err(self.too_large());
            }

            let bytes = self.channel.read(&mut buffer).await?;
            if bytes == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed before the NETCONF 1.0 message terminator",
                )
                .into());
            }
            self.read_buffer.extend_from_slice(&buffer[..bytes]);
        }
    }
}

#[async_trait]
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Framer for AsyncFramer<T> {
    async fn upgrade(&mut self) {
        self.upgraded = true;
    }

    async fn read_async(&mut self) -> NetconfClientResult<String> {
        let result = if self.upgraded {
            self.read_chunked().await
        } else {
            self.read_end_of_message().await
        };
        if result.is_err() {
            // Buffer holds a partial message the caller can no longer use.
            self.read_buffer.clear();
        }
        result
    }

    async fn write_async(&mut self, rpc: &str) -> NetconfClientResult<()> {
        debug!("RPC:\n{}", rpc);
        let bytes = rpc.as_bytes();
        if self.upgraded {
            self.channel
                .write_all(format!("\n#{}\n", bytes.len()).as_bytes())
                .await?;
            self.channel.write_all(bytes).await?;
            self.channel.write_all("\n##\n".as_bytes()).await?;
        } else {
            self.channel.write_all(bytes).await?;
            self.channel
                .write_all(NETCONF_1_0_TERMINATOR.as_bytes())
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::io::Cursor;

    #[tokio::test]
    async fn test_chunked_framer() {
        let rpc_error = r#"
#38
<?xml version="1.0" encoding="UTF-8"?>
#1


#10
<rpc-reply
#50
 message-id="8ddd59e5-96fc-4a55-a75f-a3fae2d9f712"
#48
 xmlns="urn:ietf:params:xml:ns:netconf:base:1.0"
#1
>
#1


#14
    <rpc-error
#1
>
#1


#41
        <error-type>protocol</error-type>
#1


#42
        <error-tag>bad-element</error-tag>
#1


#46
        <error-severity>error</error-severity>
#1


#22
        <error-message
#1
>
#1


#58
            Element is not valid in the specified context.
#1


#24
        </error-message>
#1


#19
        <error-info
#1
>
#1


#45
            <bad-element>startu</bad-element>
#1


#21
        </error-info>
#1


#16
    </rpc-error>
#1


#12
</rpc-reply>
##

"#
        .to_string();
        let channel = Cursor::new(rpc_error.into_bytes());
        let mut framer = AsyncFramer::new(channel);
        framer.upgrade().await;

        let resp = framer.read_async().await.unwrap();
        let expected = r#"
<?xml version="1.0" encoding="UTF-8"?>
<rpc-reply message-id="8ddd59e5-96fc-4a55-a75f-a3fae2d9f712" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
    <rpc-error>
        <error-type>protocol</error-type>
        <error-tag>bad-element</error-tag>
        <error-severity>error</error-severity>
        <error-message>
            Element is not valid in the specified context.
        </error-message>
        <error-info>
            <bad-element>startu</bad-element>
        </error-info>
    </rpc-error>
</rpc-reply>
"#;
        assert_eq!(resp, expected.trim());
    }

    #[tokio::test]
    async fn test_eof_framer() {
        let rpc_error = r#"
<?xml version="1.0" encoding="UTF-8"?>
<rpc-reply message-id="8ddd59e5-96fc-4a55-a75f-a3fae2d9f712" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
    <rpc-error>
        <error-type>protocol</error-type>
        <error-tag>bad-element</error-tag>
        <error-severity>error</error-severity>
        <error-message>
            Element is not valid in the specified context.
        </error-message>
        <error-info>
            <bad-element>startu</bad-element>
        </error-info>
    </rpc-error>
</rpc-reply>
]]>]]>"#;
        let channel = Cursor::new(rpc_error.trim().as_bytes().to_vec());
        let mut framer = AsyncFramer::new(channel);
        let resp = framer.read_async().await.unwrap();
        let expected = r#"
<?xml version="1.0" encoding="UTF-8"?>
<rpc-reply message-id="8ddd59e5-96fc-4a55-a75f-a3fae2d9f712" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
    <rpc-error>
        <error-type>protocol</error-type>
        <error-tag>bad-element</error-tag>
        <error-severity>error</error-severity>
        <error-message>
            Element is not valid in the specified context.
        </error-message>
        <error-info>
            <bad-element>startu</bad-element>
        </error-info>
    </rpc-error>
</rpc-reply>
"#;
        assert_eq!(resp, expected.trim());
    }

    #[tokio::test]
    async fn eof_without_terminator_is_an_error() {
        let channel = Cursor::new(b"<rpc-reply/>".to_vec());
        let mut framer = AsyncFramer::new(channel);
        let err = framer.read_async().await.unwrap_err();
        assert!(
            matches!(&err, NetconfClientError::Io(io) if io.kind() == io::ErrorKind::UnexpectedEof),
            "expected UnexpectedEof, got {err:?}"
        );
    }

    #[tokio::test]
    async fn terminator_split_across_reads_is_found() {
        let (client, mut server) = tokio::io::duplex(64);
        let writer = tokio::spawn(async move {
            server.write_all(b"<ok/>]]").await.unwrap();
            server.write_all(b">]]>rest").await.unwrap();
        });
        let mut framer = AsyncFramer::new(client);
        assert_eq!(framer.read_async().await.unwrap(), "<ok/>");
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn chunk_header_digits_are_bounded() {
        let channel = Cursor::new(b"\n#99999999999\nx".to_vec());
        let mut framer = AsyncFramer::new(channel);
        framer.upgrade().await;
        assert!(matches!(
            framer.read_async().await.unwrap_err(),
            NetconfClientError::MessageTooLarge { .. }
        ));
    }

    #[tokio::test]
    async fn chunk_larger_than_limit_is_rejected() {
        let channel = Cursor::new(b"\n#4000000000\n".to_vec());
        let mut framer = AsyncFramer::new(channel);
        framer.upgrade().await;
        assert!(matches!(
            framer.read_async().await.unwrap_err(),
            NetconfClientError::MessageTooLarge { .. }
        ));
    }

    #[tokio::test]
    async fn garbage_in_chunk_header_is_rejected() {
        let channel = Cursor::new(b"\n#12#34\nx".to_vec());
        let mut framer = AsyncFramer::new(channel);
        framer.upgrade().await;
        assert!(matches!(
            framer.read_async().await.unwrap_err(),
            NetconfClientError::MalformedChunk { .. }
        ));
    }

    #[tokio::test]
    async fn eof_framer_keeps_pipelined_bytes() {
        let channel = Cursor::new(b"<one/>]]>]]><two/>]]>]]>".to_vec());
        let mut framer = AsyncFramer::new(channel);
        assert_eq!(framer.read_async().await.unwrap(), "<one/>");
        assert_eq!(framer.read_async().await.unwrap(), "<two/>");
    }
}
