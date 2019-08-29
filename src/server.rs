use std::collections::HashMap;
use std::error::Error as StdError;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Poll, PollOpt, Ready, Token};
use mio_extras::channel::{Receiver, Sender};

type Result<T> = std::io::Result<T>;

pub struct ServerIncoming {
    handle: ServerHandle,
    incoming_poll: Poll,
}

pub struct Client {
    tcp_stream: TcpStream,
    handle: ServerHandle,
}

pub struct ServerStreaming {
    handle: ServerHandle,
    streaming_poll: Poll,
}

#[derive(Clone)]
struct ServerHandle {
    inner: Arc<Mutex<ServerInner>>,
    poll: Arc<Poll>,
}

struct ServerInner {
    tx: Sender<ControlEvent>,
    incoming_rx: Receiver<Token>,
    streaming_rx: Receiver<StreamingEvent>,
    tokens: Vec<Token>,
    clients: HashMap<Token, InternalClient>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

#[derive(Debug)]
enum InternalClient {
    Listener(TcpListener),
    Incoming(TcpStream, BytesMut),
    Drain(TcpStream, BytesMut),
    Streaming(TcpStream, BytesMut),
}

#[derive(Debug)]
pub enum Receive<T> {
    Receive(T),
    Invalid,
    None,
}

#[derive(Debug)]
enum ControlEvent {
    NewClient(Token, InternalClient),
    FeedStream(Token),
    Shutdown,
}

#[derive(Debug)]
pub enum StreamingEvent {
    Ready(Token),
    Close(Token),
    Shutdown,
}

pub struct Server;

impl Server {
    pub fn launch_new() -> Result<(ServerIncoming, ServerStreaming)> {
        let poll = Arc::new(Poll::new()?);
        let rx_token = Token(0);

        let (incoming_tx, incoming_rx) = mio_extras::channel::channel::<Token>();
        let incoming_poll = Poll::new()?;
        incoming_poll.register(&incoming_rx, rx_token, Ready::readable(), PollOpt::edge())?;

        let (streaming_tx, streaming_rx) = mio_extras::channel::channel::<StreamingEvent>();
        let streaming_poll = Poll::new()?;
        streaming_poll.register(&streaming_rx, rx_token, Ready::readable(), PollOpt::edge())?;

        let (server_tx, server_rx) = mio_extras::channel::channel::<ControlEvent>();
        poll.register(&server_rx, rx_token, Ready::readable(), PollOpt::edge())?;

        let inner = Arc::new(Mutex::new(ServerInner {
            tx: server_tx,
            incoming_rx,
            streaming_rx,
            tokens: (1..128).map(Token).collect(),
            clients: HashMap::new(),
            join_handle: None,
        }));

        let server_handle = ServerHandle {
            inner: inner.clone(),
            poll: poll.clone(),
        };

        let result = (
            ServerIncoming {
                handle: server_handle.clone(),
                incoming_poll,
            },
            ServerStreaming {
                handle: server_handle.clone(),
                streaming_poll,
            },
        );

        let result_inner = inner.clone();

        let join_handle = std::thread::spawn(move || {
            debug!("started");

            let mut events = Events::with_capacity(1024);

            loop {
                {
                    poll.reregister(&server_rx, rx_token, Ready::readable(), PollOpt::edge())
                        .unwrap();
                }

                poll.poll(&mut events, None).unwrap();

                for event in events.iter() {
                    let token = event.token();

                    let inner = &mut *inner.lock().unwrap();

                    if token == rx_token {
                        match server_rx.try_recv().unwrap() {
                            ControlEvent::NewClient(token, c) => match &c {
                                InternalClient::Listener(tcp_listener) => {
                                    poll.register(
                                        tcp_listener,
                                        token,
                                        Ready::readable(),
                                        PollOpt::edge(),
                                    )
                                    .unwrap();
                                    inner.clients.insert(token, c);
                                }
                                InternalClient::Drain(tcp_stream, _) => {
                                    poll.register(
                                        tcp_stream,
                                        token,
                                        Ready::writable(),
                                        PollOpt::edge(),
                                    )
                                    .unwrap();
                                    inner.clients.insert(token, c);
                                }
                                InternalClient::Streaming(tcp_stream, _) => {
                                    poll.register(
                                        tcp_stream,
                                        token,
                                        Ready::writable(),
                                        PollOpt::edge(),
                                    )
                                    .unwrap();
                                    inner.clients.insert(token, c);
                                }
                                _ => {
                                    unreachable!();
                                }
                            },
                            ControlEvent::FeedStream(token) => {
                                if let Some(InternalClient::Streaming(tcp_stream, _)) =
                                    inner.clients.get(&token)
                                {
                                    poll.reregister(
                                        tcp_stream,
                                        token,
                                        Ready::writable(),
                                        PollOpt::edge(),
                                    )
                                    .unwrap();
                                }
                            }
                            ControlEvent::Shutdown => {
                                debug!("shutdown command received");
                                streaming_tx.send(StreamingEvent::Shutdown).unwrap();
                                return;
                            }
                        }
                        continue;
                    }

                    let client = inner
                        .clients
                        .get_mut(&token)
                        .expect("token without matching client from poll");

                    match client {
                        InternalClient::Listener(tcp_listener) => {
                            let (tcp_stream, _address) = match tcp_listener.accept() {
                                Ok(c) => c,
                                Err(e) => {
                                    error!("tcp accept error: {}", e.description());
                                    continue;
                                }
                            };

                            let token = match inner.tokens.pop() {
                                Some(t) => t,
                                None => {
                                    error!("max connections reached");
                                    continue;
                                }
                            };

                            poll.register(&tcp_stream, token, Ready::readable(), PollOpt::edge())
                                .unwrap();

                            inner.clients.insert(
                                token,
                                InternalClient::Incoming(tcp_stream, BytesMut::new()),
                            );
                        }
                        InternalClient::Incoming(tcp_stream, in_buf) => {
                            let mut buf = [0; 1024];
                            let n = match tcp_stream.read(&mut buf) {
                                Ok(n) => n,
                                Err(e) => {
                                    error!("tcp read error: {}", e.description());
                                    poll.deregister(tcp_stream).unwrap();
                                    inner.clients.remove(&token);
                                    inner.tokens.push(token);
                                    continue;
                                }
                            };

                            in_buf.extend_from_slice(&buf[0..n]);
                            incoming_tx.send(token).unwrap();
                        }
                        InternalClient::Drain(tcp_stream, out_buf) => {
                            let n = match tcp_stream.write(&out_buf) {
                                Ok(n) => n,
                                Err(e) => {
                                    error!("tcp write error: {}", e.description());
                                    poll.deregister(tcp_stream).unwrap();
                                    inner.clients.remove(&token);
                                    inner.tokens.push(token);
                                    continue;
                                }
                            };

                            out_buf.advance(n);

                            if out_buf.is_empty() {
                                poll.deregister(tcp_stream).unwrap();
                                inner.clients.remove(&token);
                                inner.tokens.push(token);
                            }
                        }
                        InternalClient::Streaming(tcp_stream, out_buf) => {
                            let n = match tcp_stream.write(&out_buf) {
                                Ok(n) => n,
                                Err(e) => {
                                    error!("tcp write error: {}", e.description());
                                    streaming_tx.send(StreamingEvent::Close(token)).unwrap();
                                    poll.deregister(tcp_stream).unwrap();
                                    inner.clients.remove(&token);
                                    inner.tokens.push(token);
                                    continue;
                                }
                            };

                            out_buf.advance(n);

                            if out_buf.is_empty() {
                                poll.reregister(tcp_stream, token, Ready::empty(), PollOpt::edge())
                                    .unwrap();
                                streaming_tx.send(StreamingEvent::Ready(token)).unwrap();
                            }
                        }
                    }
                }
            }
        });

        result_inner.lock().unwrap().join_handle = Some(join_handle);

        Ok(result)
    }
}

impl ServerIncoming {
    pub fn add_listener(&self, tcp_listener: TcpListener) -> Result<()> {
        let inner = &mut *self.handle.inner.lock().unwrap();

        let token = match inner.tokens.pop() {
            Some(t) => t,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "max connections reached",
                ));
            }
        };

        inner
            .tx
            .send(ControlEvent::NewClient(
                token,
                InternalClient::Listener(tcp_listener),
            ))
            .unwrap();

        Ok(())
    }

    pub fn receive_next_fn<F, T>(&self, process: F) -> std::io::Result<(Client, T)>
    where
        F: Fn(&[u8]) -> Receive<T>,
    {
        let mut events = Events::with_capacity(32);
        loop {
            self.incoming_poll.poll(&mut events, None)?;

            let inner = &mut *self.handle.inner.lock().unwrap();
            let token = inner.incoming_rx.try_recv().unwrap();
            let client = inner.clients.get_mut(&token).unwrap();

            if let InternalClient::Incoming(tcp_stream, bytes) = client {
                match process(bytes) {
                    Receive::Receive(v) => {
                        if let InternalClient::Incoming(tcp_stream, _) =
                            inner.clients.remove(&token).unwrap()
                        {
                            self.handle.poll.deregister(&tcp_stream).unwrap();
                            inner.tokens.push(token);

                            let client = Client {
                                handle: self.handle.clone(),
                                tcp_stream,
                            };

                            return Ok((client, v));
                        } else {
                            unreachable!();
                        }
                    }
                    Receive::Invalid => {
                        self.handle.poll.deregister(tcp_stream).unwrap();
                        inner.clients.remove(&token).unwrap();
                        inner.tokens.push(token);
                    }
                    Receive::None => {}
                }
            }
        }
    }
}

impl Client {
    pub fn send(self, data: &[u8]) -> std::io::Result<()> {
        let inner = &mut *self.handle.inner.lock().unwrap();

        let token = match inner.tokens.pop() {
            Some(t) => t,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "max connections reached",
                ));
            }
        };

        inner
            .tx
            .send(ControlEvent::NewClient(
                token,
                InternalClient::Drain(self.tcp_stream, BytesMut::from(data)),
            ))
            .unwrap();

        Ok(())
    }

    pub fn add_stream(self, out_buf: BytesMut) -> std::io::Result<Token> {
        let inner = &mut *self.handle.inner.lock().unwrap();

        let token = match inner.tokens.pop() {
            Some(t) => t,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "max connections reached",
                ));
            }
        };

        inner
            .tx
            .send(ControlEvent::NewClient(
                token,
                InternalClient::Streaming(self.tcp_stream, out_buf),
            ))
            .unwrap();

        Ok(token)
    }
}

impl ServerStreaming {
    pub fn streaming_next(&self) -> Result<StreamingEvent> {
        let mut events = Events::with_capacity(32);

        {
            let inner = &mut *self.handle.inner.lock().unwrap();
            self.streaming_poll.reregister(
                &inner.streaming_rx,
                Token(0),
                Ready::readable(),
                PollOpt::edge(),
            )?;
        }

        self.streaming_poll.poll(&mut events, None).unwrap();

        let inner = &mut *self.handle.inner.lock().unwrap();
        let event = inner.streaming_rx.try_recv().unwrap();

        Ok(event)
    }

    pub fn streaming_feed(&self, token: Token, data: &[u8]) {
        let inner = &mut *self.handle.inner.lock().unwrap();
        if let Some(client) = inner.clients.get_mut(&token) {
            if let InternalClient::Streaming(_, out_buf) = client {
                let n = out_buf.len();

                out_buf.extend_from_slice(data);

                if n == 0 {
                    inner.tx.send(ControlEvent::FeedStream(token)).unwrap();
                }
            }
        }
    }

    pub fn streaming_drain(&self, token: Token) {
        let inner = &mut *self.handle.inner.lock().unwrap();

        if let Some(InternalClient::Streaming(tcp_stream, out_buf)) = inner.clients.remove(&token) {
            self.handle.poll.deregister(&tcp_stream).unwrap();
            inner
                .tx
                .send(ControlEvent::NewClient(
                    token,
                    InternalClient::Drain(tcp_stream, out_buf),
                ))
                .unwrap();
        }
    }
}