use std::sync::{Arc, Mutex};

use bytes::BytesMut;

use crate::audio_stream::AudioStream;
use crate::http::HttpResponse;
use crate::server::{
    IncomingClient, ServerStreaming, StreamingClient, StreamingClientStatus, StreamingEvent,
};

type Result<T> = std::io::Result<T>;

pub struct StreamThread {
    inner: Arc<Mutex<StreamThreadInner>>,
}

pub struct StreamThreadInner {
    audio_streams: Vec<(StreamingClient, AudioStream)>,
}

impl StreamThread {
    pub fn launch_new(server: ServerStreaming) -> Result<StreamThread> {
        let inner = Arc::new(Mutex::new(StreamThreadInner {
            audio_streams: Vec::new(),
        }));

        let result_inner = inner.clone();

        std::thread::spawn(move || {
            debug!("started");

            loop {
                let event = server.streaming_next().unwrap();

                match event {
                    StreamingEvent::Event => {}
                    StreamingEvent::Shutdown => {
                        // TODO
                        break;
                    }
                }

                let inner = &mut *inner.lock().unwrap();

                let mut remove_indices: Vec<usize> = Vec::new();

                for (index, (client, audio_stream)) in inner.audio_streams.iter_mut().enumerate() {
                    match client.status() {
                        StreamingClientStatus::Waiting => {
                            continue;
                        }
                        StreamingClientStatus::Closed => {
                            remove_indices.push(index);
                        }
                        StreamingClientStatus::Ready => {
                            let mut buf = BytesMut::new();

                            let mut result = true;

                            while result && buf.len() < 10 * 1024 {
                                result = audio_stream.next(|data| {
                                    buf.extend_from_slice(&data);
                                    data.len()
                                });
                            }

                            trace!("read {} bytes from audio stream, feeding", buf.len());

                            if result {
                                client.feed(&buf);
                            } else {
                                debug!("draining audio stream");

                                client.drain(&buf);
                                remove_indices.push(index);
                            }
                        }
                    }
                }

                for index in remove_indices.iter().rev() {
                    inner.audio_streams.remove(*index);
                    debug!("removed audio stream");
                }
            }

            debug!("stopping")
        });

        Ok(StreamThread {
            inner: result_inner,
        })
    }

    pub fn add_audio_stream(
        &self,
        client: IncomingClient,
        audio_stream: AudioStream,
    ) -> Result<()> {
        let inner = &mut *self.inner.lock().unwrap();

        let mut response = HttpResponse::new();
        response.content_type("audio/mpeg");

        let client = client.into_stream(response.to_bytes())?;
        inner.audio_streams.push((client, audio_stream));

        debug!("added audio stream");

        Ok(())
    }
}
