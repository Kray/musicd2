use std::collections::{BTreeMap, HashMap};

use hyper::header::ToStrError;
use hyper::HeaderMap;

pub fn parse_cookies(headers: &HeaderMap) -> Result<HashMap<String, String>, ToStrError> {
    let mut cookies: HashMap<String, String> = HashMap::new();

    if let Some(cookie_header) = headers.get("Cookie") {
        match cookie_header.to_str() {
            Ok(cookie_headers) => {
                for c in cookie_headers.split(';') {
                    let mut parts = c.split('=');
                    cookies.insert(
                        parts.next().unwrap().to_string(),
                        parts.next().unwrap_or_default().to_string(),
                    );
                }
            }
            Err(e) => {
                return Err(e);
            }
        }
    }

    Ok(cookies)
}

#[derive(Debug)]
pub struct HttpQuery {
    value: BTreeMap<String, String>,
}

impl HttpQuery {
    pub fn from(s: &str) -> HttpQuery {
        let mut query = HttpQuery {
            value: BTreeMap::new(),
        };

        for field in s.split('&') {
            let mut parts = field.splitn(2, '=');

            let key = parts.next().unwrap();
            let value = Self::decode_url(parts.next().unwrap_or(""));

            query
                .value
                .insert(key.to_string(), Self::decode_url(&value));
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
                    result.push(u8::from_str_radix(&src[i + 1..i + 3], 16).unwrap());
                    i += 3;
                }
                b'+' => {
                    result.push(b' ');
                    i += 1;
                }
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
