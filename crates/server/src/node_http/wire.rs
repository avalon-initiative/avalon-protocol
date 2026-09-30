//! Wire format and limits for node-to-node HTTP exchanges carried over a libp2p stream.
//!
//! One request-response exchange per stream, framed as `u32 BE header length | JSON header |
//! u32 BE body length | body`. Every length is checked before any allocation, so an oversized
//! frame is refused while it is read and never buffered past its limit.

use std::io;
use std::time::Duration;

use libp2p::futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::StreamProtocol;
use serde::{Deserialize, Serialize};

use crate::relay::bounded_env;

const PROTOCOL_BASE: &str = "/avalon/node-http/1.0.0";

const DEFAULT_MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;
const DEFAULT_MAX_INFLIGHT: u64 = 64;
const MAX_MAX_INFLIGHT: u64 = 1024;
const DEFAULT_MAX_INFLIGHT_PER_PEER: u64 = 8;
const MAX_MAX_INFLIGHT_PER_PEER: u64 = 256;

/// Most header lines and their total name plus value bytes in one message.
pub const MAX_HEADER_COUNT: usize = 32;
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_METHOD_BYTES: usize = 16;
/// Ceiling on the JSON header frame itself, generous for escaping of `MAX_HEADER_BYTES`.
const MAX_HEAD_FRAME_BYTES: usize = 96 * 1024;

/// The stream protocol id, scoped to `network_id` so nodes of other networks never negotiate it.
pub fn protocol_name(network_id: &str) -> StreamProtocol {
    StreamProtocol::try_from_owned(format!("{PROTOCOL_BASE}/{network_id}"))
        .expect("a network_id-scoped protocol string always starts with '/'")
}

/// Limits and timeouts for the stream transport, all finite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeHttpSettings {
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub timeout: Duration,
    pub max_inflight: usize,
    pub max_inflight_per_peer: usize,
}

impl Default for NodeHttpSettings {
    fn default() -> Self {
        Self {
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES as usize,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES as usize,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            max_inflight: DEFAULT_MAX_INFLIGHT as usize,
            max_inflight_per_peer: DEFAULT_MAX_INFLIGHT_PER_PEER as usize,
        }
    }
}

impl NodeHttpSettings {
    /// Every knob is at least 1 and has a ceiling; an out-of-range value is a startup error.
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            max_request_bytes: bounded_env(
                "AVALON_NODE_HTTP_MAX_REQUEST_BYTES",
                DEFAULT_MAX_REQUEST_BYTES,
                MAX_MAX_REQUEST_BYTES,
            )? as usize,
            max_response_bytes: bounded_env(
                "AVALON_NODE_HTTP_MAX_RESPONSE_BYTES",
                DEFAULT_MAX_RESPONSE_BYTES,
                MAX_MAX_RESPONSE_BYTES,
            )? as usize,
            timeout: Duration::from_secs(bounded_env(
                "AVALON_NODE_HTTP_TIMEOUT_SECS",
                DEFAULT_TIMEOUT_SECS,
                MAX_TIMEOUT_SECS,
            )?),
            max_inflight: bounded_env(
                "AVALON_NODE_HTTP_MAX_INFLIGHT",
                DEFAULT_MAX_INFLIGHT,
                MAX_MAX_INFLIGHT,
            )? as usize,
            max_inflight_per_peer: bounded_env(
                "AVALON_NODE_HTTP_MAX_INFLIGHT_PER_PEER",
                DEFAULT_MAX_INFLIGHT_PER_PEER,
                MAX_MAX_INFLIGHT_PER_PEER,
            )? as usize,
        })
    }
}

/// An HTTP request as carried over a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeHttpRequest {
    pub method: String,
    pub path_and_query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// An HTTP response as carried over a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl NodeHttpResponse {
    /// A short JSON error body, the shape the HTTP handlers use.
    pub fn error(status: u16, code: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: format!("{{\"error\":\"{code}\"}}").into_bytes(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RequestHead {
    method: String,
    path_and_query: String,
    headers: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize)]
struct ResponseHead {
    status: u16,
    headers: Vec<(String, String)>,
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

pub(crate) fn check_headers(headers: &[(String, String)]) -> io::Result<()> {
    if headers.len() > MAX_HEADER_COUNT {
        return Err(invalid("too many headers"));
    }
    let bytes: usize = headers.iter().map(|(k, v)| k.len() + v.len()).sum();
    if bytes > MAX_HEADER_BYTES {
        return Err(invalid("headers too large"));
    }
    Ok(())
}

/// Reads exactly `len` bytes, growing the buffer only as data arrives.
async fn read_bounded<T: AsyncRead + Unpin + Send>(io: &mut T, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(len.min(64 * 1024));
    let n = (&mut *io).take(len as u64).read_to_end(&mut buf).await?;
    if n != len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(buf)
}

async fn read_len<T: AsyncRead + Unpin + Send>(io: &mut T) -> io::Result<usize> {
    let mut raw = [0u8; 4];
    io.read_exact(&mut raw).await?;
    Ok(u32::from_be_bytes(raw) as usize)
}

/// Reads `head frame | body`, refusing a body over `max_body` before reading any of it.
async fn read_frame<T: AsyncRead + Unpin + Send>(
    io: &mut T,
    max_body: usize,
) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let head_len = read_len(io).await?;
    if head_len > MAX_HEAD_FRAME_BYTES {
        return Err(invalid("header frame too large"));
    }
    let head = read_bounded(io, head_len).await?;
    let body_len = read_len(io).await?;
    if body_len > max_body {
        return Err(invalid("body too large"));
    }
    let body = read_bounded(io, body_len).await?;
    Ok((head, body))
}

async fn write_frame<T: AsyncWrite + Unpin + Send>(
    io: &mut T,
    head: &[u8],
    body: &[u8],
) -> io::Result<()> {
    if head.len() > MAX_HEAD_FRAME_BYTES {
        return Err(invalid("header frame too large"));
    }
    io.write_all(&(head.len() as u32).to_be_bytes()).await?;
    io.write_all(head).await?;
    io.write_all(&(body.len() as u32).to_be_bytes()).await?;
    io.write_all(body).await?;
    io.flush().await
}

/// The request-response codec for [`protocol_name`]; the limits are this node's own.
#[derive(Debug, Clone)]
pub struct NodeHttpCodec {
    max_request_bytes: usize,
    max_response_bytes: usize,
}

impl NodeHttpCodec {
    pub fn new(settings: &NodeHttpSettings) -> Self {
        Self {
            max_request_bytes: settings.max_request_bytes,
            max_response_bytes: settings.max_response_bytes,
        }
    }
}

impl libp2p::request_response::Codec for NodeHttpCodec {
    type Protocol = StreamProtocol;
    type Request = NodeHttpRequest;
    type Response = NodeHttpResponse;

    async fn read_request<T>(&mut self, _: &Self::Protocol, io: &mut T) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let (head, body) = read_frame(io, self.max_request_bytes).await?;
        let head: RequestHead =
            serde_json::from_slice(&head).map_err(|e| invalid(e.to_string()))?;
        if head.path_and_query.len() > MAX_PATH_BYTES || head.method.len() > MAX_METHOD_BYTES {
            return Err(invalid("request line too large"));
        }
        check_headers(&head.headers)?;
        Ok(NodeHttpRequest {
            method: head.method,
            path_and_query: head.path_and_query,
            headers: head.headers,
            body,
        })
    }

    async fn read_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        let (head, body) = read_frame(io, self.max_response_bytes).await?;
        let head: ResponseHead =
            serde_json::from_slice(&head).map_err(|e| invalid(e.to_string()))?;
        if !(100..=599).contains(&head.status) {
            return Err(invalid("status out of range"));
        }
        check_headers(&head.headers)?;
        Ok(NodeHttpResponse {
            status: head.status,
            headers: head.headers,
            body,
        })
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        if req.body.len() > self.max_request_bytes {
            return Err(invalid("request body too large"));
        }
        if req.path_and_query.len() > MAX_PATH_BYTES || req.method.len() > MAX_METHOD_BYTES {
            return Err(invalid("request line too large"));
        }
        check_headers(&req.headers)?;
        let head = serde_json::to_vec(&RequestHead {
            method: req.method,
            path_and_query: req.path_and_query,
            headers: req.headers,
        })
        .map_err(|e| invalid(e.to_string()))?;
        write_frame(io, &head, &req.body).await
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        if res.body.len() > self.max_response_bytes {
            return Err(invalid("response body too large"));
        }
        check_headers(&res.headers)?;
        let head = serde_json::to_vec(&ResponseHead {
            status: res.status,
            headers: res.headers,
        })
        .map_err(|e| invalid(e.to_string()))?;
        write_frame(io, &head, &res.body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::futures::io::Cursor;
    use libp2p::request_response::Codec;

    fn codec(max_req: usize, max_res: usize) -> NodeHttpCodec {
        NodeHttpCodec {
            max_request_bytes: max_req,
            max_response_bytes: max_res,
        }
    }

    fn proto() -> StreamProtocol {
        protocol_name("net")
    }

    fn sample_request() -> NodeHttpRequest {
        NodeHttpRequest {
            method: "POST".into(),
            path_and_query: "/nodes/announce?a=1".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: b"{\"x\":1}".to_vec(),
        }
    }

    async fn encode_request(c: &mut NodeHttpCodec, r: NodeHttpRequest) -> Vec<u8> {
        let mut out = Vec::new();
        c.write_request(&proto(), &mut out, r).await.unwrap();
        out
    }

    #[tokio::test]
    async fn request_and_response_round_trip() {
        let mut c = codec(1024, 1024);
        let bytes = encode_request(&mut c, sample_request()).await;
        let got = c
            .read_request(&proto(), &mut Cursor::new(bytes))
            .await
            .unwrap();
        assert_eq!(got, sample_request());

        let res = NodeHttpResponse {
            status: 201,
            headers: vec![("x-a".into(), "b".into())],
            body: vec![1, 2, 3],
        };
        let mut out = Vec::new();
        c.write_response(&proto(), &mut out, res.clone())
            .await
            .unwrap();
        let got = c
            .read_response(&proto(), &mut Cursor::new(out))
            .await
            .unwrap();
        assert_eq!(got, res);
    }

    #[tokio::test]
    async fn truncated_frames_are_errors() {
        let mut c = codec(1024, 1024);
        let bytes = encode_request(&mut c, sample_request()).await;
        for cut in [0, 2, 6, bytes.len() - 1] {
            let err = c
                .read_request(&proto(), &mut Cursor::new(bytes[..cut].to_vec()))
                .await
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "cut at {cut}");
        }
    }

    /// Reader that records how many bytes were pulled from it.
    struct Counting<'a> {
        data: Cursor<Vec<u8>>,
        read: &'a std::sync::atomic::AtomicUsize,
    }

    impl AsyncRead for Counting<'_> {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<io::Result<usize>> {
            let poll = std::pin::Pin::new(&mut self.data).poll_read(cx, buf);
            if let std::task::Poll::Ready(Ok(n)) = &poll {
                self.read.fetch_add(*n, std::sync::atomic::Ordering::SeqCst);
            }
            poll
        }
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused_before_it_is_read() {
        let mut c = codec(16, 16);
        let mut frame = Vec::new();
        let head = serde_json::to_vec(&RequestHead {
            method: "POST".into(),
            path_and_query: "/x".into(),
            headers: vec![],
        })
        .unwrap();
        frame.extend((head.len() as u32).to_be_bytes());
        frame.extend(&head);
        frame.extend(1_000_000u32.to_be_bytes());
        frame.extend(vec![7u8; 1_000_000]);

        let read = std::sync::atomic::AtomicUsize::new(0);
        let mut io = Counting {
            data: Cursor::new(frame),
            read: &read,
        };
        let err = c.read_request(&proto(), &mut io).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            read.load(std::sync::atomic::Ordering::SeqCst) < 1024,
            "the body must not be read"
        );
    }

    #[tokio::test]
    async fn an_oversized_response_body_is_refused_before_it_is_read() {
        let mut c = codec(16, 16);
        let head = serde_json::to_vec(&ResponseHead {
            status: 200,
            headers: vec![],
        })
        .unwrap();
        let mut frame = Vec::new();
        frame.extend((head.len() as u32).to_be_bytes());
        frame.extend(&head);
        frame.extend(17u32.to_be_bytes());
        frame.extend(vec![0u8; 17]);
        let read = std::sync::atomic::AtomicUsize::new(0);
        let mut io = Counting {
            data: Cursor::new(frame),
            read: &read,
        };
        assert!(c.read_response(&proto(), &mut io).await.is_err());
        assert!(read.load(std::sync::atomic::Ordering::SeqCst) < 64);
    }

    #[tokio::test]
    async fn an_oversized_header_frame_is_refused_without_reading_it() {
        let mut c = codec(1024, 1024);
        let mut frame = Vec::new();
        frame.extend(((MAX_HEAD_FRAME_BYTES + 1) as u32).to_be_bytes());
        frame.extend(vec![b' '; MAX_HEAD_FRAME_BYTES + 1]);
        let read = std::sync::atomic::AtomicUsize::new(0);
        let mut io = Counting {
            data: Cursor::new(frame),
            read: &read,
        };
        assert!(c.read_request(&proto(), &mut io).await.is_err());
        assert!(read.load(std::sync::atomic::Ordering::SeqCst) < 16);
    }

    #[tokio::test]
    async fn header_count_and_byte_limits_apply_on_both_sides() {
        let mut c = codec(1024, 1024);
        let mut many = sample_request();
        many.headers = (0..=MAX_HEADER_COUNT)
            .map(|i| (format!("h{i}"), "v".to_string()))
            .collect();
        assert!(c
            .write_request(&proto(), &mut Vec::new(), many)
            .await
            .is_err());

        let mut big = sample_request();
        big.headers = vec![("x".into(), "v".repeat(MAX_HEADER_BYTES + 1))];
        assert!(c
            .write_request(&proto(), &mut Vec::new(), big)
            .await
            .is_err());

        // A hostile peer skips the writer's check and sends too many headers.
        let head = serde_json::to_vec(&RequestHead {
            method: "GET".into(),
            path_and_query: "/x".into(),
            headers: (0..=MAX_HEADER_COUNT)
                .map(|i| (format!("h{i}"), "v".to_string()))
                .collect(),
        })
        .unwrap();
        let mut frame = Vec::new();
        frame.extend((head.len() as u32).to_be_bytes());
        frame.extend(&head);
        frame.extend(0u32.to_be_bytes());
        assert!(c
            .read_request(&proto(), &mut Cursor::new(frame))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn bad_json_and_bad_status_are_errors() {
        let mut c = codec(1024, 1024);
        let head = b"not json";
        let mut frame = Vec::new();
        frame.extend((head.len() as u32).to_be_bytes());
        frame.extend(head);
        frame.extend(0u32.to_be_bytes());
        assert!(c
            .read_request(&proto(), &mut Cursor::new(frame.clone()))
            .await
            .is_err());
        assert!(c
            .read_response(&proto(), &mut Cursor::new(frame))
            .await
            .is_err());

        let head = serde_json::to_vec(&ResponseHead {
            status: 99,
            headers: vec![],
        })
        .unwrap();
        let mut frame = Vec::new();
        frame.extend((head.len() as u32).to_be_bytes());
        frame.extend(&head);
        frame.extend(0u32.to_be_bytes());
        assert!(c
            .read_response(&proto(), &mut Cursor::new(frame))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn the_writer_refuses_an_oversized_body() {
        let mut c = codec(4, 4);
        let mut r = sample_request();
        r.body = vec![0; 5];
        assert!(c.write_request(&proto(), &mut Vec::new(), r).await.is_err());
    }

    #[test]
    fn the_protocol_name_is_namespaced_by_network_id() {
        assert_ne!(protocol_name("a").as_ref(), protocol_name("b").as_ref());
        assert_eq!(protocol_name("net").as_ref(), "/avalon/node-http/1.0.0/net");
    }

    #[test]
    fn settings_env_knobs_are_bounded() {
        let _env = crate::test_env::guard();
        unsafe {
            std::env::remove_var("AVALON_NODE_HTTP_MAX_REQUEST_BYTES");
        }
        assert_eq!(
            NodeHttpSettings::from_env().unwrap(),
            NodeHttpSettings::default()
        );
        unsafe {
            std::env::set_var("AVALON_NODE_HTTP_MAX_REQUEST_BYTES", "0");
        }
        assert!(NodeHttpSettings::from_env().is_err());
        unsafe {
            std::env::set_var(
                "AVALON_NODE_HTTP_MAX_REQUEST_BYTES",
                (MAX_MAX_REQUEST_BYTES + 1).to_string(),
            );
        }
        assert!(NodeHttpSettings::from_env().is_err());
        unsafe {
            std::env::set_var("AVALON_NODE_HTTP_MAX_REQUEST_BYTES", "2048");
        }
        assert_eq!(
            NodeHttpSettings::from_env().unwrap().max_request_bytes,
            2048
        );
        unsafe {
            std::env::remove_var("AVALON_NODE_HTTP_MAX_REQUEST_BYTES");
        }
    }
}
