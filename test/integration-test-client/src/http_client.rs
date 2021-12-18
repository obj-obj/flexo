use std::io::prelude::*;
use std::net::{TcpStream};
use sha2::{Sha256, Digest};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Debug, Hash, Clone, PartialEq, Eq)]
pub struct ConnAddr {
    pub host: String,
    pub port: u16,
}

pub struct GetRequest {
    pub path: String,
    pub client_header: ClientHeader,
}

pub struct GetRequestTest {
    pub conn_addr: ConnAddr,
    pub get_requests: Vec<GetRequest>,
    pub timeout: Option<Duration>,
}

pub enum ClientHeader {
    AutoGenerated,
    Custom(String),
}

const BUF_SIZE: usize = 4096;

const HEADER_SEPARATOR: &[u8; 4] = b"\r\n\r\n";
pub const HEADER_SEPARATOR_STR: &str = "\r\n\r\n";

#[derive(Debug, Hash, Clone, PartialEq, Eq)]
pub struct HeaderResult {
    pub status_code: u32,
    pub content_length: usize,
    pub cached: bool,
}

#[derive(Debug, Hash, Clone, PartialEq, Eq)]
pub struct BodyResult {
    pub sha: Vec<u8>,
    pub size: usize,
}

#[derive(Debug, Hash, Clone, PartialEq, Eq)]
pub struct HttpGetResult {
    pub header_result: HeaderResult,
    pub payload_result: Option<BodyResult>,
}

pub fn http_get(request: GetRequestTest, testcase: &'static str) -> Option<Vec<HttpGetResult>> {
    http_get_with_header_chunked(request, None, testcase)
}

pub fn http_get_with_header_chunked(
    request_test: GetRequestTest,
    maybe_pattern: Option<ChunkPattern>,
    testcase: &'static str,
) -> Option<Vec<HttpGetResult>> {
    let host = request_test.conn_addr.host.clone();
    let (sender, receiver) = mpsc::channel::<Vec<HttpGetResult>>();
    let timeout = request_test.timeout.unwrap_or(Duration::from_millis(5000));
    thread::spawn(move || {
        let conn_addr = request_test.conn_addr.clone();
        info!("{}: Connecting to {:?}", testcase, conn_addr);
        let mut stream = TcpStream::connect((conn_addr.host, conn_addr.port)).unwrap();
        info!("{}: Connection established.", testcase);
        let results = request_test.get_requests.iter().map(|request| {
            let header: String = match &request.client_header {
                ClientHeader::AutoGenerated =>
                    format!("GET {} HTTP/1.1\r\nHost: {}{}", request.path, request_test.conn_addr.host, HEADER_SEPARATOR_STR),
                ClientHeader::Custom(h) => h.clone(),
            };
            let pattern = maybe_pattern.unwrap_or_else(|| ChunkPattern {
                chunk_size: header.len(),
                wait_interval: Duration::from_millis(0),
            });
            let header_bytes = header.as_bytes();
            for header_chunk in header_bytes.chunks(pattern.chunk_size) {
                stream.write(header_chunk).unwrap();
            }
            info!("{}: Sending header.", testcase);
            let header_result = read_header(&mut stream);
            info!("{}: Successfully fetched header: {:?}", testcase, header_result);
            let payload_result = match header_result.content_length {
                0 => {
                    info!("{}: Content length has size zero", testcase);
                    None
                },
                content_length => {
                    info!("{}: Successfully fetched payload, size is {}", testcase, content_length);
                    Some(body_result(&mut stream, content_length))
                },
            };
            HttpGetResult {
                header_result,
                payload_result,
            }
        }).collect::<Vec<HttpGetResult>>();
        info!("{}: Sending results back to main thread", testcase);
        sender.send(results)
    });
    info!("{}: Waiting for response from thread for request at host {}, Timeout is {:?}", testcase, host, timeout);
    match receiver.recv_timeout(timeout) {
        Ok(r) => {
            info!("{}: Got response", testcase);
            Some(r)
        },
        Err(_) => {
            info!("{}: No response received within {:?}", testcase, timeout);
            None
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkPattern {
    pub chunk_size: usize,
    pub wait_interval: Duration,
}

fn read_header(stream: &mut TcpStream) -> HeaderResult {
    let payload = &mut[0; BUF_SIZE];
    let mut size_read = 0;
    loop {
        match stream.read(&mut payload[size_read..size_read + 1]) {
            Ok(1) => {
                size_read += 1;
                if size_read >= HEADER_SEPARATOR.len() {
                    if &payload[size_read - HEADER_SEPARATOR.len()..size_read] == HEADER_SEPARATOR {
                        break;
                    }
                }
            },
            Ok(s) => panic!("Unexpected size while reading from socket: {}", s),
            Err(e) => panic!("Unable to read header: {:?}", e),
        }
    }
    let header_bytes = &payload[..size_read - HEADER_SEPARATOR.len()];
    let content_length = content_length(header_bytes);
    let status_code = status_code(header_bytes);
    let cached = cached(header_bytes);
    HeaderResult {
        status_code,
        content_length,
        cached,
    }
}

fn body_result(stream: &mut TcpStream, content_length: usize) -> BodyResult {
    let mut hasher = Sha256::new();
    let payload = &mut[0; BUF_SIZE];
    let mut size_read = 0;
    while size_read < content_length {
        match stream.read(payload) {
            Ok(size) => {
                size_read += size;
                hasher.update(&payload[..size]);
            }
            Err(e) => panic!("Unable to read body: {:?}", e),
        }
    }
    BodyResult {
        sha: hasher.finalize().to_vec(),
        size: size_read,
    }
}

fn content_length(header: &[u8]) -> usize {
    let content_length = get_header_value(header, b"Content-Length: ");
    content_length.parse::<usize>().unwrap()
}

fn cached(header: &[u8]) -> bool {
    let payload_origin = get_header_value(header, b"Flexo-Payload-Origin: ");
    payload_origin == "Cache"
}

fn status_code(header: &[u8]) -> u32 {
    let keyword = b" ";
    let start_idx = header
        .iter()
        .position(|header_part| header_part == &b' ')
        .unwrap() + keyword.len();
    let end_idx = header[start_idx..]
        .iter()
        .position(|header_part| header_part == &b' ')
        .unwrap() + start_idx;
    let status_code = &header[start_idx..end_idx];
    std::string::String::from_utf8(Vec::from(status_code)).unwrap().parse::<u32>().unwrap()
}

fn get_header_value(header: &[u8], keyword: &[u8]) -> String {
    let start_idx = header
        .windows(keyword.len())
        .position(|header_part| header_part == keyword).unwrap() + keyword.len();
    let end_idx = header[start_idx..]
        .iter()
        .position(|header_part| header_part == &b'\r').map(|i| i + start_idx)
        .unwrap_or(header.len());
    let content_length = &header[start_idx..end_idx];
    std::string::String::from_utf8(Vec::from(content_length)).unwrap()
}
