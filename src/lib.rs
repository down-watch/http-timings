#![deny(missing_docs)]
#![deny(missing_debug_implementations)]
#![deny(rustdoc::all)]
#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![deny(clippy::cargo)]

//! # http-timings
//!
//! `http-timings` is a simple library to measure the key HTTP timings
//! from the [dev-tools](https://developer.chrome.com/docs/devtools/network/reference/?utm_source=devtools#timing-explanation)
//!
//! ## Usage
//! ```
//! use http_timings::request_url;
//!
//! if let Some(timings) = request_url("https://www.google.com") {
//!    println!("{:?}", timings); // Outputs a [`RequestOutput`] struct
//! }
//! ```

use std::{
    error::Error,
    fmt::Debug,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::Duration,
};

use flate2::read::{DeflateDecoder, GzDecoder};
use rustls_connector::RustlsConnector;
use url::Url;

trait ReadWrite: Read + Write + Debug {}
impl<T: Read + Write + Send + Sync + Debug> ReadWrite for T {}

/// A pair of durations
///
/// The `total` field is the sum of the durations up to that step
/// The `relative` field is the duration of the step itself
#[derive(Debug)]
pub struct DurationPair {
    total: Duration,
    relative: Duration,
}

impl DurationPair {
    /// Get the total duration
    #[must_use]
    pub fn total(&self) -> Duration {
        self.total
    }

    /// Get the relative duration
    #[must_use]
    pub fn relative(&self) -> Duration {
        self.relative
    }
}

/// The key HTTP timings for a request
#[derive(Debug)]
pub struct RequestTimings {
    dns: Duration,
    tcp: Duration,
    tls: Option<Duration>,
    http_send: Duration,
    ttfb: Duration,
    content_download: Duration,
}

impl RequestTimings {
    /// Create a new instance of [`RequestTimings`]
    ///
    /// All durations are relative, the [`DurationPair`] is called to get the total duration
    #[must_use]
    pub fn new(
        dns: Duration,
        tcp: Duration,
        tls: Option<Duration>,
        http_send: Duration,
        ttfb: Duration,
        content_download: Duration,
    ) -> Self {
        Self {
            dns,
            tcp,
            tls,
            http_send,
            ttfb,
            content_download,
        }
    }

    /// Get the DNS timings
    #[must_use]
    pub fn dns(&self) -> DurationPair {
        DurationPair {
            total: self.dns,
            relative: self.dns,
        }
    }

    /// Get the TCP timings
    ///
    /// # Panics
    /// This will panic if the total duration overflows
    #[must_use]
    pub fn tcp(&self) -> DurationPair {
        DurationPair {
            total: self.dns().total.checked_add(self.tcp).unwrap(),
            relative: self.tcp,
        }
    }

    /// Get the TLS timings
    ///
    /// # Panics
    /// This will panic if the total duration overflows
    #[must_use]
    pub fn tls(&self) -> Option<DurationPair> {
        self.tls.map(|duration| DurationPair {
            total: self.tcp().total.checked_add(duration).unwrap(),
            relative: duration,
        })
    }

    /// Get the HTTP Send timings
    ///
    /// # Panics
    /// This will panic if the total duration overflows
    #[must_use]
    pub fn http_send(&self) -> DurationPair {
        DurationPair {
            total: self
                .tls()
                .map_or_else(|| self.tcp().total, |tls| tls.total)
                .checked_add(self.http_send)
                .unwrap(),
            relative: self.http_send,
        }
    }

    /// Get the Time To First Byte timings
    ///
    /// # Panics
    /// This will panic if the total duration overflows
    #[must_use]
    pub fn ttfb(&self) -> DurationPair {
        DurationPair {
            total: self.http_send.checked_add(self.ttfb).unwrap(),
            relative: self.ttfb,
        }
    }

    /// Get the Content Download timings
    ///
    /// # Panics
    /// This will panic if the total duration overflows
    #[must_use]
    pub fn content_download(&self) -> DurationPair {
        DurationPair {
            total: self
                .ttfb()
                .total
                .checked_add(self.content_download)
                .unwrap(),
            relative: self.content_download,
        }
    }

    /// Get the total duration
    #[must_use]
    pub fn total(&self) -> Duration {
        self.content_download().total
    }
}

/// Output structure for a call to [`request_url`]
#[derive(Debug)]
pub struct RequestOutput {
    status: u16,
    timings: RequestTimings,
    body: String,
}

impl RequestOutput {
    /// Get the status code
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Get the timings
    #[must_use]
    pub fn timings(&self) -> &RequestTimings {
        &self.timings
    }

    /// Get the body
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

fn get_dns_timing(url: &Url) -> Result<Duration, Box<dyn Error>> {
    let Some(domain) = url.host_str() else {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid URL",
        )));
    };
    let port = url.port().unwrap_or(match url.scheme() {
        "http" => 80,
        "https" => 443,
        _ => {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid scheme",
            )))
        }
    });
    let now = std::time::Instant::now();
    match format!("{domain}:{port}").to_socket_addrs() {
        Ok(_) => {}
        Err(e) => return Err(Box::new(e)),
    };
    Ok(now.elapsed())
}

fn get_tcp_timing(
    url: &Url,
    max_duration: Option<Duration>,
) -> Result<(Box<dyn ReadWrite + Send + Sync>, Duration), Box<dyn Error>> {
    // Unwrap is safe here because we know the URL is valid from the DNS timing
    let host = url.host_str().unwrap();
    let now = std::time::Instant::now();
    let stream = match TcpStream::connect(format!("{host}:443")) {
        Ok(stream) => stream,
        Err(e) => return Err(Box::new(e)),
    };
    stream.set_read_timeout(max_duration)?;
    Ok((Box::new(stream), now.elapsed()))
}

fn get_tls_timing(
    url: &Url,
    stream: Box<dyn ReadWrite + Send + Sync>,
) -> Result<(Box<dyn ReadWrite + Send + Sync>, Duration), Box<dyn Error>> {
    let connector = RustlsConnector::new_with_webpki_roots_certs();
    let now = std::time::Instant::now();
    let stream = match connector.connect(url.host_str().unwrap(), stream) {
        Ok(stream) => stream,
        Err(e) => {
            return Err(Box::new(e));
        }
    };
    Ok((Box::new(stream), now.elapsed()))
}

fn get_http_send_timing(
    url: &Url,
    stream: &mut Box<dyn ReadWrite + Send + Sync>,
) -> Result<Duration, Box<dyn Error>> {
    let header = format!("GET {} HTTP/1.0\r\nHost: {}\r\nAccept-Encoding: gzip, deflate, br\r\nUser-Agent: http-timings/0.1\r\nConnection: keep-alive\r\nAccept: */*\r\n\r\n", url.path(), url.host().unwrap());
    let now = std::time::Instant::now();
    if let Err(e) = stream.write_all(header.as_bytes()) {
        return Err(Box::new(e));
    };
    Ok(now.elapsed())
}

fn get_ttfb_timing(
    stream: &mut Box<dyn ReadWrite + Send + Sync>,
) -> Result<Duration, Box<dyn Error>> {
    let mut one_byte_buf = [0_u8];
    let now = std::time::Instant::now();
    if let Err(e) = stream.read_exact(&mut one_byte_buf) {
        return Err(Box::new(e));
    };
    Ok(now.elapsed())
}

fn get_content_download_timing(
    stream: &mut Box<dyn ReadWrite + Send + Sync>,
) -> Result<(u16, Duration, String), Box<dyn Error>> {
    let mut reader = BufReader::new(stream);
    let mut header_buf = String::new();
    let now = std::time::Instant::now();
    loop {
        let bytes_read = match reader.read_line(&mut header_buf) {
            Ok(bytes_read) => bytes_read,
            Err(e) => return Err(Box::new(e)),
        };
        if bytes_read == 2 {
            break;
        }
    }
    let headers = header_buf.split('\n');
    let content_length = match headers
        .clone()
        .filter(|line| line.starts_with("Content-Length"))
        .collect::<Vec<_>>()
        .first()
    {
        Some(content_length) => content_length.split(':').collect::<Vec<_>>()[1]
            .trim()
            .parse()
            .unwrap_or(0),
        None => 0,
    };

    let mut body_buf;
    if content_length == 0 {
        body_buf = vec![];
        if let Err(e) = reader.read_to_end(&mut body_buf) {
            return Err(Box::new(e));
        }
    } else {
        body_buf = vec![0_u8; content_length];
        if let Err(e) = reader.read_exact(&mut body_buf) {
            return Err(Box::new(e));
        };
    }

    let content_download_time = now.elapsed();

    let status = match headers
        .clone()
        .filter(|line| line.starts_with("TTP"))
        .collect::<Vec<_>>()
        .first()
    {
        Some(status) => status.split(' ').collect::<Vec<_>>()[1].parse::<u16>()?,
        None => {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "No status code returned",
            )))
        }
    };

    let content_encoding = match headers
        .filter(|line| line.starts_with("Content-Encoding"))
        .collect::<Vec<_>>()
        .first()
    {
        Some(content_encoding) => content_encoding.split(':').collect::<Vec<_>>()[1].trim(),
        None => "",
    };

    let body = match content_encoding {
        "gzip" => {
            let decoder = GzDecoder::new(&body_buf[..]);
            let mut decode_reader = BufReader::new(decoder);
            let mut buf = vec![];
            let _ = decode_reader.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        }
        "deflate" => {
            let mut decoder = DeflateDecoder::new(&body_buf[..]);
            let mut string = String::new();
            if let Err(e) = decoder.read_to_string(&mut string) {
                return Err(Box::new(e));
            }
            string
        }
        "br" => {
            let mut decoder = brotli::Decompressor::new(&body_buf[..], 4096);
            let mut buf = vec![];
            if let Err(e) = decoder.read_to_end(&mut buf) {
                return Err(Box::new(e));
            }
            String::from_utf8_lossy(&buf).into_owned()
        }
        _ => String::from_utf8_lossy(&body_buf).into_owned(),
    };
    Ok((status, content_download_time, body))
}

fn get_request_output(
    url_input: impl AsRef<str>,
    max_duration: Option<Duration>,
) -> Result<RequestOutput, Box<dyn Error>> {
    let input = url_input.as_ref();
    if input.is_empty() {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Missing URL input",
        )));
    }
    let input = input.to_string();
    let input = if input.starts_with("http") {
        input
    } else {
        format!("http://{input}")
    };

    let url = match Url::parse(input.as_str()) {
        Ok(url) => url,
        Err(e) => return Err(Box::new(e)),
    };

    let dns = get_dns_timing(&url)?;
    let (stream, tcp) = get_tcp_timing(&url, max_duration)?;
    let (mut stream, tls) = get_tls_timing(&url, stream)?;
    let http_send = get_http_send_timing(&url, &mut stream)?;
    let ttfb = get_ttfb_timing(&mut stream)?;
    let (status, content_download, body) = get_content_download_timing(&mut stream)?;

    Ok(RequestOutput {
        status,
        timings: RequestTimings::new(
            dns,
            tcp,
            match url.scheme() {
                "https" => Some(tls),
                _ => None,
            },
            http_send,
            ttfb,
            content_download,
        ),
        body,
    })
}

/// Get the HTTP timings, status and body for a given URL
///
/// # Errors
/// This will error if:
/// - The URL is invalid
/// - Any timing step fails
/// - Decoding the body fails
pub fn request_url(url_input: impl AsRef<str>) -> Result<RequestOutput, Box<dyn Error>> {
    get_request_output(url_input, None)
}

/// Get the HTTP timings, status and body for a given URL with a timeout
///
/// # Errors
/// This will error if:
/// - The URL is invalid
/// - Any timing step fails
/// - Decoding the body fails
pub fn request_url_with_timeout(
    url_input: impl AsRef<str>,
    max_duration: Duration,
) -> Result<RequestOutput, Box<dyn Error>> {
    get_request_output(url_input, Some(max_duration))
}

#[cfg(test)]
mod test {
    use crate::request_url;

    #[test]
    fn test_non_tls_connection() {
        let url = "neverssl.com";
        let output = request_url(url).unwrap();
        assert_eq!(output.status(), 200);
        assert!(output.body().contains("Follow @neverssl"));
        assert!(output.timings().dns().total().as_secs() < 1);
        assert!(output.timings().content_download().total().as_secs() < 5);
    }

    #[test]
    fn test_popular_tls_connection() {
        let url = "https://www.google.com";
        let output = request_url(url).unwrap();
        assert_eq!(output.status(), 200);
        assert!(output.body().contains("Google Search"));
        assert!(output.timings().dns().total().as_secs() < 1);
        assert!(output.timings().content_download().total().as_secs() < 5);
    }

    #[test]
    fn test_ip() {
        let url = "1.1.1.1";
        let output = request_url(url).unwrap();
        assert_eq!(output.status(), 302);
        assert!(output.body().is_empty());
        assert!(output.timings().dns().total().as_secs() < 1);
        assert!(output.timings().content_download().total().as_secs() < 5);
    }

    #[test]
    fn i_need_this_rq() {
        println!("{:?}", request_url("https://www.google.com").unwrap());
    }
}
