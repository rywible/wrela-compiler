//! # HTTP
//!
//! A server build that a host serves over HTTP (language.md §19.5) reads each request as a short
//! connection: a `Connection` event that opens it, then one `Message` whose bytes are the
//! request ([`Request`]), and it answers once with the `http/respond` command ([`SERVICE_RESPOND`]:
//! the connection's number, then the response's bytes, [`Response`]). The host closes the
//! connection once the response goes, the client goes, or [`RESPOND_WITHIN_MS`] pass, and the
//! program hears so on a later call. A client asks its page's own origin with the `http/fetch`
//! service ([`SERVICE_FETCH`]), whose request is a [`Request`] and whose answer a [`Response`].
//! A server asks the time of day with `time/unix` ([`SERVICE_TIME`]): eight bytes, the
//! milliseconds since the Unix epoch.
//!
//! The bytes, every number little-endian:
//!
//! | Request | Response |
//! |---|---|
//! | [`REQUEST_MAGIC`] | [`RESPONSE_MAGIC`] |
//! | the time it was taken, a `u64` of Unix milliseconds (0 from a client) | the status, a `u32` |
//! | the method, the path, the query: each a `u32` length, then UTF-8 | the headers |
//! | the headers: a `u32` count, then each name (lower case) and value as the method is | the body: a `u32` length, then bytes |
//! | the body: a `u32` length, then bytes | |
//!
//! What crosses is allowlisted, so no address reaches a program: a server sees only the request
//! headers [`seen_by_server`] takes, sets only those [`set_by_server`] takes, and a client sets
//! and sees those of [`set_by_client`] and [`seen_by_client`].

/// What a request's bytes start with.
pub const REQUEST_MAGIC: [u8; 4] = *b"WHQ1";
/// What a response's bytes start with.
pub const RESPONSE_MAGIC: [u8; 4] = *b"WHR1";
/// The command a server answers a request with.
pub const SERVICE_RESPOND: &str = "http/respond";
/// The service a client asks its page's origin with.
pub const SERVICE_FETCH: &str = "http/fetch";
/// The service that answers the time of day.
pub const SERVICE_TIME: &str = "time/unix";
/// The most bytes a request is, encoded: one message, and one host request.
pub const MAX_REQUEST: usize = 1 << 20;
/// How long a request waits for its response before the host answers 504 for it.
pub const RESPOND_WITHIN_MS: u32 = 30_000;
/// The methods a host takes.
pub const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
/// The methods a host takes from any origin: they change nothing (RFC 9110 §9.2.1).
pub const SAFE_METHODS: [&str; 3] = ["GET", "HEAD", "OPTIONS"];
/// A program's own headers, which cross every way.
pub const OWN_PREFIX: &str = "x-wrela-";
/// The request headers a server sees, besides its own. Never an address, a place or a device.
pub const SERVER_SEES: [&str; 10] = [
    "accept",
    "accept-language",
    "content-type",
    "cookie",
    "if-none-match",
    "origin",
    "sec-fetch-dest",
    "sec-fetch-mode",
    "sec-fetch-site",
    "sec-fetch-user",
];
/// The response headers a server sets, besides its own.
pub const SERVER_SETS: [&str; 10] = [
    "cache-control",
    "content-disposition",
    "content-type",
    "etag",
    "last-modified",
    "location",
    "retry-after",
    "set-cookie",
    "vary",
    "www-authenticate",
];
/// The request headers a client sets, besides its own.
pub const CLIENT_SETS: [&str; 3] = ["accept", "content-type", "if-none-match"];
/// The response headers a client sees, besides its own: never `set-cookie`, which the browser
/// keeps.
pub const CLIENT_SEES: [&str; 5] =
    ["cache-control", "content-type", "etag", "location", "retry-after"];

fn listed(list: &[&str], name: &str) -> bool {
    name.starts_with(OWN_PREFIX) || list.contains(&name)
}

/// Whether a server sees request header `name` (lower case).
pub fn seen_by_server(name: &str) -> bool {
    listed(&SERVER_SEES, name)
}

/// Whether a server may set response header `name`.
pub fn set_by_server(name: &str) -> bool {
    listed(&SERVER_SETS, name)
}

/// Whether a client may set request header `name`.
pub fn set_by_client(name: &str) -> bool {
    listed(&CLIENT_SETS, name)
}

/// Whether a client sees response header `name`.
pub fn seen_by_client(name: &str) -> bool {
    listed(&CLIENT_SEES, name)
}

/// A request, as a server reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// When the host took it, in Unix milliseconds; 0 from a client.
    pub time: u64,
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A response, as a server writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u32,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

fn counted(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn headers(out: &mut Vec<u8>, headers: &[(String, String)]) {
    out.extend_from_slice(&(headers.len() as u32).to_le_bytes());
    for (name, value) in headers {
        counted(out, name.as_bytes());
        counted(out, value.as_bytes());
    }
}

/// Bytes being read, with why they don't read.
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err("the bytes end early".into());
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn word(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")))
    }

    fn text(&mut self) -> Result<String, String> {
        let n = self.word()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "text that isn't UTF-8".to_string())
    }

    fn headers(&mut self) -> Result<Vec<(String, String)>, String> {
        let n = self.word()?;
        // Each header is at least its two lengths.
        if n as usize > self.0.len() / 8 {
            return Err("more headers than bytes".into());
        }
        (0..n).map(|_| Ok((self.text()?, self.text()?))).collect()
    }
}

impl Request {
    /// Its bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = REQUEST_MAGIC.to_vec();
        out.extend_from_slice(&self.time.to_le_bytes());
        for part in [&self.method, &self.path, &self.query] {
            counted(&mut out, part.as_bytes());
        }
        headers(&mut out, &self.headers);
        counted(&mut out, &self.body);
        out
    }

    /// A request from its bytes, or why they aren't one.
    pub fn decode(bytes: &[u8]) -> Result<Request, String> {
        let mut r = Bytes(bytes);
        if r.take(4)? != REQUEST_MAGIC {
            return Err("not a request".into());
        }
        let time = u64::from_le_bytes(r.take(8)?.try_into().expect("eight bytes"));
        let (method, path, query) = (r.text()?, r.text()?, r.text()?);
        let headers = r.headers()?;
        let n = r.word()? as usize;
        let body = r.take(n)?.to_vec();
        if !r.0.is_empty() {
            return Err("bytes past the request's end".into());
        }
        Ok(Request { time, method, path, query, headers, body })
    }
}

impl Response {
    /// Its bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = RESPONSE_MAGIC.to_vec();
        out.extend_from_slice(&self.status.to_le_bytes());
        headers(&mut out, &self.headers);
        counted(&mut out, &self.body);
        out
    }

    /// A response from its bytes, or why they aren't one.
    pub fn decode(bytes: &[u8]) -> Result<Response, String> {
        let mut r = Bytes(bytes);
        if r.take(4)? != RESPONSE_MAGIC {
            return Err("not a response".into());
        }
        let status = r.word()?;
        if !(200..=599).contains(&status) {
            return Err(format!("status {status} isn't 200 to 599"));
        }
        let headers = r.headers()?;
        let n = r.word()? as usize;
        let body = r.take(n)?.to_vec();
        if !r.0.is_empty() {
            return Err("bytes past the response's end".into());
        }
        Ok(Response { status, headers, body })
    }
}

/// The vectors both sides read: a request and a response, with their bytes.
pub fn examples() -> (Request, Response) {
    let request = Request {
        time: 1_793_404_800_123,
        method: "POST".into(),
        path: "/api/sign-in".into(),
        query: "step=2".into(),
        headers: vec![
            ("content-type".into(), "application/octet-stream".into()),
            ("cookie".into(), "__Host-session=abc; theme=dark".into()),
            ("x-wrela-csrf".into(), "7f".into()),
        ],
        body: vec![1, 2, 3],
    };
    let response = Response {
        status: 201,
        headers: vec![
            ("content-type".into(), "text/plain; charset=utf-8".into()),
            (
                "set-cookie".into(),
                "__Host-session=abc; Path=/; Secure; HttpOnly; SameSite=Strict".into(),
            ),
        ],
        body: b"made".to_vec(),
    };
    (request, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_and_a_response_read_back_and_cut_ones_do_not() {
        let (request, response) = examples();
        let q = request.encode();
        assert_eq!(&q[..4], b"WHQ1");
        assert_eq!(Request::decode(&q), Ok(request));
        let r = response.encode();
        assert_eq!(Response::decode(&r), Ok(response));
        for cut in [3, 12, q.len() - 1] {
            assert!(Request::decode(&q[..cut]).is_err(), "cut at {cut}");
        }
        assert!(Request::decode(&[&q[..], &[0]].concat()).is_err(), "a byte past its end");
        assert!(Request::decode(&r).is_err(), "a response isn't a request");
        let mut bad = r.clone();
        bad[4..8].copy_from_slice(&600u32.to_le_bytes());
        assert!(Response::decode(&bad).is_err(), "a status past 599");
    }

    #[test]
    fn what_crosses_is_allowlisted_and_never_an_address() {
        for address in
            ["cf-connecting-ip", "x-forwarded-for", "x-real-ip", "true-client-ip", "cf-ipcountry"]
        {
            assert!(!seen_by_server(address), "{address}");
        }
        assert!(seen_by_server("cookie") && seen_by_server("x-wrela-csrf"));
        assert!(set_by_server("set-cookie") && !set_by_server("access-control-allow-origin"));
        assert!(!seen_by_client("set-cookie") && seen_by_client("x-wrela-page"));
        assert!(set_by_client("content-type") && !set_by_client("cookie"));
    }
}
