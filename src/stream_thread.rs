use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use mio::Token;

use crate::audio_stream::AudioStream;
use crate::http::HttpResponse;
use crate::server::{Client, ServerStreaming, StreamingEvent};

type Result<T> = std::io::Result<T>;

pub struct StreamThread {
    inner: Arc<Mutex<StreamThreadInner>>,
}

pub struct StreamThreadInner {
    audio_streams: HashMap<Token, AudioStream>,
}

impl StreamThread {
    pub fn launch_new(server: ServerStreaming) -> Result<StreamThread> {
        let inner = Arc::new(Mutex::new(StreamThreadInner {
            audio_streams: HashMap::new(),
        }));

        let result_inner = inner.clone();

        std::thread::spawn(move || {
            debug!("started");

            loop {
                let event = server.streaming_next().unwrap();

                let token = match event {
                    StreamingEvent::Ready(t) => t,
                    StreamingEvent::Close(t) => {
                        let _audio_stream = inner.lock().unwrap().audio_streams.remove(&t).unwrap();
                        debug!("closing audio stream {:?}", t);
                        continue;
                    }
                    StreamingEvent::Shutdown => {
                        debug!("shutdown command received");
                        break;
                    }
                };

                let inner = &mut *inner.lock().unwrap();
                let audio_stream = inner
                    .audio_streams
                    .get_mut(&token)
                    .expect("stream_thread nonexistent audio stream reported as writable");

                let mut buf = BytesMut::new();

                let result = audio_stream.next(|data| {
                    buf.extend_from_slice(&data);
                    data.len()
                });

                trace!(
                    "received {} bytes from audio stream {:?}, feeding",
                    buf.len(),
                    token
                );

                if result {
                    server.streaming_feed(token, &buf);
                } else {
                    debug!("draining audio stream {:?}", token);

                    server.streaming_drain(token, &buf);
                    inner.audio_streams.remove(&token).unwrap();
                }
            }

            debug!("stopping")
        });

        Ok(StreamThread {
            inner: result_inner,
        })
    }

    pub fn add_audio_stream(&self, client: Client, audio_stream: AudioStream) -> Result<()> {
        let inner = &mut *self.inner.lock().unwrap();

        let mut response = HttpResponse::new();
        response.content_type("audio/mpeg");

        let token = client.add_stream(response.to_bytes())?;
        inner.audio_streams.insert(token, audio_stream);

        debug!("added audio stream {:?}", token);

        Ok(())
    }
}
