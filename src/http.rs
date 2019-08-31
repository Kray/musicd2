use std::collections::BTreeMap;
use std::error::Error as StdError;

use bytes::BytesMut;

fn is_complete_header(buf: &[u8]) -> bool {
    buf.len() >= 4
        && buf[buf.len() - 1] == b'\n'
        && buf[buf.len() - 2] == b'\r'
        && buf[buf.len() - 3] == b'\n'
        && buf[buf.len() - 4] == b'\r'
}

pub fn parse_request_headers(buf: &[u8]) -> std::io::Result<Option<HttpRequest>> {
    if !is_complete_header(buf) {
        return Ok(None);
    }

    let header_text = match std::str::from_utf8(buf) {
        Ok(s) => s,
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "http headers invalid",
            ));
        }
    };

    let mut raw_headers = header_text.split("\r\n");

    let request_line = match raw_headers.next() {
        Some(s) => s,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "http headers invalid",
            ));
        }
    };

    let request_parts: Vec<&str> = request_line.split(' ').collect();
    if request_parts.len() != 3 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "http headers invalid",
        ));
    }

    let full_path = request_parts[1].to_string();

    let mut path_parts = full_path.splitn(2, '?');
    let path = path_parts.next().unwrap().to_string();
    let query = HttpQuery::from(path_parts.next().unwrap_or(""));

    Ok(Some(HttpRequest {
        full_path,
        path: path.to_string(),
        query,
    }))
}

#[derive(Debug)]
pub struct HttpRequest {
    full_path: String,
    path: String,
    query: HttpQuery,
}

impl HttpRequest {
    pub fn full_path(&self) -> &str {
        &self.full_path
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn query(&self) -> &HttpQuery {
        &self.query
    }
}

pub struct HttpResponse {
    status: Option<String>,
    content_type: Option<String>,
    body: Option<BytesMut>,
}

impl HttpResponse {
    pub fn new() -> HttpResponse {
        HttpResponse {
            status: None,
            content_type: None,
            body: None,
        }
    }

    pub fn status(&mut self, status: &str) -> &mut Self {
        self.status = Some(status.to_string());
        self
    }

    pub fn content_type(&mut self, content_type: &str) -> &mut Self {
        self.content_type = Some(content_type.to_string());
        self
    }

    pub fn text_body(&mut self, body: &str) -> &mut Self {
        self.body = Some(BytesMut::from(body.as_bytes()));
        self
    }

    pub fn bytes_body(&mut self, body: &[u8]) -> &mut Self {
        self.body = Some(BytesMut::from(body));
        self
    }

    pub fn to_bytes(&self) -> BytesMut {
        let mut headers = String::new();

        headers += "HTTP/1.1 ";

        headers += match &self.status {
            Some(s) => s,
            None => "200 OK",
        };

        if let Some(ref b) = self.body {
            headers += "\r\nContent-Length: ";
            headers += &b.len().to_string();
        }

        headers += "\r\nContent-Type: ";
        headers += match &self.content_type {
            Some(t) => t,
            None => "text/plain; charset=utf-8",
        };
        headers += "\r\n\r\n";

        let mut bytes = BytesMut::new();
        bytes.extend_from_slice(headers.as_bytes());

        if let Some(body_bytes) = &self.body {
            bytes.extend_from_slice(&body_bytes);
        }

        bytes
    }
}

#[derive(Debug)]
pub struct HttpQuery {
    value: BTreeMap<String, String>,
}

impl HttpQuery {
    fn from(s: &str) -> HttpQuery {
        let mut query = HttpQuery {
            value: BTreeMap::new(),
        };

        for field in s.split('&') {
            let mut parts = field.splitn(2, '=');

            let key = parts.next().unwrap();
            let value = Self::decode_url(parts.next().unwrap_or(""));

            query.value.insert(key.to_string(), Self::decode_url(&value));
        }

        query
    }

    fn decode_url(src: &str) -> String {
        let mut result: Vec<u8> = Vec::new();
        let bytes = src.as_bytes();

        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'%' if i <= bytes.len() - 3
                    && bytes[i + 1].is_ascii_hexdigit()
                    && bytes[i + 2].is_ascii_hexdigit() =>
                {
                    result.push(u8::from_str_radix(&src[i+1..i+3], 16).unwrap());
                    i += 3;
                },
                b'+' => {
                    result.push(b' ');
                    i += 1;
                },
                ch => {
                    result.push(ch);
                    i += 1;
                }
            }
        }

        String::from_utf8_lossy(&result).into_owned()
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.value.get(key) {
            Some(s) => Some(s),
            None => None,
        }
    }

    pub fn get_i64(&self, key: &str) -> Option<i64> {
        match self.get_str(key) {
            Some(s) => match s.parse() {
                Ok(v) => Some(v),
                Err(_) => None,
            },
            None => None,
        }
    }
}

#[derive(Debug)]
pub struct HttpError {
    code: i32,
    description: String,
}

impl HttpError {
    pub fn new(code: i32, description: &str) -> HttpError {
        HttpError {
            code,
            description: description.to_string(),
        }
    }

    pub fn to_string(&self) -> String {
        format!("{} {}", self.code, self.description)
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{} {}", self.code, self.description())
    }
}

impl StdError for HttpError {
    fn description(&self) -> &str {
        &self.description
    }
}
