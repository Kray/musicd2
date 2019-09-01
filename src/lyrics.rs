use std::result::Result;

use curl::{
    easy::{Easy2, Handler, WriteError},
    Error,
};

struct Collector(Vec<u8>);

impl Handler for Collector {
    fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        self.0.extend_from_slice(data);
        Ok(data.len())
    }
}

#[derive(Debug)]
pub struct Lyrics {
    pub lyrics: String,
    pub provider: String,
    pub source: String,
}

pub fn try_fetch_lyrics(artist: &str, title: &str) -> Result<Option<Lyrics>, Error> {
    let mut curl = Easy2::new(Collector(Vec::new()));

    let url = &format!("https://lyrics.fandom.com/wiki/{}:{}", artist, title);

    debug!("fetching url {}", url);

    curl.url(url)?;
    curl.follow_location(true)?;
    curl.max_redirections(10)?;
    curl.perform()?;

    let body = curl.get_ref();
    let body = String::from_utf8_lossy(&body.0);

    Ok(match parse_lyricwiki_lyrics(&body) {
        Some(lyrics) => Some(Lyrics {
            lyrics,
            provider: "LyricWiki".to_owned(),
            source: url.to_string(),
        }),
        None => None,
    })
}

fn parse_lyricwiki_lyrics(body: &str) -> Option<String> {
    let begin_pattern = "<div class='lyricbox'>";
    let end_pattern = "<div class='lyricsbreak'>";

    let begin_index = match body.find(begin_pattern) {
        Some(i) => i + begin_pattern.len(),
        None => {
            trace!("begin '{}' not found", begin_pattern);
            return None;
        }
    };

    let end_index = match body.find(end_pattern) {
        Some(i) => i,
        None => {
            trace!("end '{}' not found", end_pattern);
            return None;
        }
    };

    let mut result = String::new();

    let body = &body[begin_index..end_index];

    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'&' if i <= bytes.len() - 3 && bytes[i + 1] == b'#' => {
                let mut i2 = i + 2;
                while i2 < bytes.len() && bytes[i2] != b';' {
                    i2 += 1;
                }

                if let Ok(s) = std::str::from_utf8(&bytes[i + 2..i2]) {
                    if let Ok(cp) = s.parse() {
                        if let Some(ch) = std::char::from_u32(cp) {
                            result.push(ch);
                        }
                    }
                }

                i = i2;
            }
            b'<' => {
                let mut i2 = i;
                while i2 < bytes.len() && bytes[i2] != b'>' {
                    i2 += 1;
                }

                if let Ok(s) = std::str::from_utf8(&bytes[i..=i2]) {
                    if s == "<br />" {
                        result.push('\n')
                    }
                }

                i = i2 + 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    Some(result)
}

#[test]
fn test1() {
    let lyrics = try_fetch_lyrics("TWRP", "ICQ").unwrap().unwrap();
    println!("{}", lyrics.lyrics);

    let lyrics = try_fetch_lyrics("広瀬香美", "Promise").unwrap().unwrap();
    println!("{}", lyrics.lyrics);
}
