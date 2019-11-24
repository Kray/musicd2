use std::result::Result;

use reqwest::Error;

#[derive(Debug)]
pub struct Lyrics {
    pub lyrics: String,
    pub provider: String,
    pub source: String,
}

pub async fn try_fetch_lyrics(artist: &str, title: &str) -> Result<Option<Lyrics>, Error> {
    Ok(try_lyricwiki_lyrics(artist, title).await?)
}

async fn try_lyricwiki_lyrics(artist: &str, title: &str) -> Result<Option<Lyrics>, Error> {
    // Try exact page
    let url = &format!("https://lyrics.fandom.com/wiki/{}:{}", artist, title);
    debug!("fetching url {}", &url);
    if let Some(l) = parse_lyricwiki_lyrics(&reqwest::get(url).await?.text().await?) {
        return Ok(Some(Lyrics {
            lyrics: l,
            provider: "LyricWiki".to_owned(),
            source: url.to_string(),
        }));
    }

    // Fetch list of all pages associated with this artist
    let url = &format!(
        "http://lyrics.wikia.com/api.php?func=getArtist&artist={}&fmt=text",
        artist
    );
    debug!("fetching url {}", url);
    let song_list = reqwest::get(url).await?.text().await?;

    // Try to find the exact page name
    if let Some(t) = song_list.split('\n').find(|t| t.ends_with(title)) {
        let url = &format!("https://lyrics.fandom.com/wiki/{}", t);
        debug!("fetching url {}", url);
        if let Some(l) = parse_lyricwiki_lyrics(&reqwest::get(url).await?.text().await?) {
            return Ok(Some(Lyrics {
                lyrics: l,
                provider: "LyricWiki".to_owned(),
                source: url.to_string(),
            }));
        }
    }

    // Try to find the primary artist name and use it
    if let Some(artist) = song_list.split(':').next() {
        let url = &format!("https://lyrics.fandom.com/wiki/{}:{}", artist, title);
        debug!("fetching url {}", url);
        if let Some(l) = parse_lyricwiki_lyrics(&reqwest::get(url).await?.text().await?) {
            return Ok(Some(Lyrics {
                lyrics: l,
                provider: "LyricWiki".to_owned(),
                source: url.to_string(),
            }));
        }
    }

    Ok(None)
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
