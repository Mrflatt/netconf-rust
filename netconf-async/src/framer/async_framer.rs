use crate::error::{NetconfClientError, NetconfClientResult};
use crate::framer::{Framer, NETCONF_1_0_TERMINATOR};
use async_trait::async_trait;
use log::debug;
use memmem::{Searcher, TwoWaySearcher};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Trait for NETCONF message framing
/// See [RFC6242](https://tools.ietf.org/html/rfc6242#section-4.1)
pub struct AsyncFramer<T> {
    read_buffer: Vec<u8>,
    upgraded: AtomicBool,

    channel: T,
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncFramer<T> {
    pub fn new(channel: T) -> Self {
        AsyncFramer {
            read_buffer: Vec::new(),
            upgraded: AtomicBool::new(false),
            channel,
        }
    }

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
        let mut last_read: u8;
        loop {
            let mut buffer = [0u8; 1];
            self.channel.read_exact(&mut buffer).await?;
            last_read = buffer[0];
            if last_read == b'#' {
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
            chunk_size = chunk_size * 10 + u32::from(last_read - b'0');
        }
    }
}

#[async_trait]
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Framer for AsyncFramer<T> {
    async fn upgrade(&mut self) {
        self.upgraded.store(true, Ordering::Relaxed);
    }

    async fn read_async(&mut self) -> NetconfClientResult<String> {
        if self.upgraded.load(Ordering::Relaxed) {
            loop {
                let chunk_size: u32 = self.read_header().await?;
                if chunk_size == 0 {
                    break;
                }
                let mut buffer = vec![0u8; chunk_size as usize];
                self.channel.read_exact(&mut buffer).await?;
                self.read_buffer.extend(&buffer);
            }
            let response = String::from_utf8_lossy(&self.read_buffer)
                .trim_end()
                .to_string();
            self.read_buffer.drain(..);
            Ok(response)
        } else {
            let mut buffer = [0u8; 256];
            let search = TwoWaySearcher::new(NETCONF_1_0_TERMINATOR.as_bytes());
            while search.search_in(&self.read_buffer).is_none() {
                let bytes = self.channel.read(&mut buffer).await?;
                self.read_buffer.extend(&buffer[..bytes]);
            }
            let pos = search.search_in(&self.read_buffer).unwrap();
            let resp = String::from_utf8_lossy(&self.read_buffer[..pos])
                .trim_end()
                .to_string();
            self.read_buffer.drain(0..(pos + 6));
            Ok(resp)
        }
    }

    async fn write_async(&mut self, rpc: &str) -> NetconfClientResult<()> {
        debug!("RPC:\n{}", rpc);
        let bytes = rpc.as_bytes();
        if self.upgraded.load(Ordering::Relaxed) {
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
}
