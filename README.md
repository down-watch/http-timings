# http-timings: A library to measure HTTP timings
Inspired by the [TTFB](https://github.com/phip1611/ttfb) library by [phip1611](https://github.com/phip1611). The HTTP timings provided by this library are as follows:
- DNS Lookup Time
- TCP Connection Time
- TLS Handshake Time
- HTTP Send Time
- Time to First Byte
- Content Download Time

As well as this timing, this library also provides the following information about each request:
- HTTP Status Code
- Body of the response

## Usage
```rust
use http_timings::request_url;

if let Ok(timings) = request_url("https://www.google.com") {
   println!("{:?}", timings);
}

/// Output:
/// RequestOutput {
///   status: 200,
///   timings: RequestTimings {
///     dns: 19.121396ms,
///     tcp: 42.066481ms,
///     tls: Some(29.665676ms),
///     http_send: 8.977µs,
///     ttbf: 101.531718ms,
///     content_download: 2.268473ms
///   },
///   body: "<!doctype html>..."
/// }
```

The `RequestOutput` struct provides the timings in both relative and total terms. The relative timings are the time taken for each step of the request, while the total timings are the time taken from the start of the request to the end of the request.

The URL input can be any valid website as well as any valid IP.