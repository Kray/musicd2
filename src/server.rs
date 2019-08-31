use std::collections::HashMap;
use std::error::Error as StdError;
use std::io::{Read, Write};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex, Weak};

use bytes::BytesMut;
use mio::net::{TcpListener, TcpStream};
use mio::{Event, Events, Poll, PollOpt, Ready, Token};
use mio_extras::channel::{Receiver, SendError, Sender};

type Result<T> = std::io::Result<T>;

pub struct Server;

pub struct ServerIncoming {
    incoming_poll: Poll,
    incoming_rx: Receiver<InternalIncomingEvent>,
    server_tx: Arc<Mutex<Sender<InternalCommand>>>,
}

pub struct ServerStreaming {
    streaming_poll: Poll,
    streaming_rx: Receiver<StreamingEvent>,
    _server_tx: Arc<Mutex<Sender<InternalCommand>>>,
}

pub struct IncomingClient {
    internal: Arc<Mutex<InternalClient>>,
}

pub struct StreamingClient {
    internal: Arc<Mutex<InternalClient>>,
}

#[derive(Debug)]
pub enum Receive<T> {
    Receive(T),
    Invalid,
    None,
}

pub enum IncomingResult<T> {
    Request(IncomingClient, T),
    Shutdown,
}

#[derive(Debug)]
pub enum StreamingEvent {
    Event,
    Shutdown,
}

#[derive(Debug)]
pub enum StreamingClientStatus {
    Ready,
    Closed,
    Waiting,
}

struct ServerThread {
    clients: HashMap<Token, Arc<Mutex<InternalClient>>>,
    server_tx: Weak<Mutex<Sender<InternalCommand>>>,
    server_rx: Receiver<InternalCommand>,
    incoming_tx: Sender<InternalIncomingEvent>,
    streaming_tx: Sender<StreamingEvent>,
    poll: Poll,
    tokens: Vec<Token>,
    listeners: HashMap<Token, InternalListener>,
    rx_token: Token,
}

#[derive(Debug)]
struct InternalListener {
    token: Token,
    listener: TcpListener,
}

struct InternalClient {
    token: Option<Token>,
    state: InternalClientState,
    stream: TcpStream,
    buffer: BytesMut,
    server_tx: Arc<Mutex<Sender<InternalCommand>>>,
}

#[derive(Debug)]
enum InternalClientState {
    Incoming,
    Waiting,
    Drain,
    Streaming,
}

enum InternalCommand {
    AddListener(TcpListener),
    Close(Arc<Mutex<InternalClient>>),
    Waiting(Arc<Mutex<InternalClient>>),
    Drain(Arc<Mutex<InternalClient>>, BytesMut),
    Stream(Arc<Mutex<InternalClient>>, BytesMut),
    StreamFeed(Arc<Mutex<InternalClient>>, BytesMut),
    StreamDrain(Arc<Mutex<InternalClient>>, BytesMut),
    Shutdown,
}

enum InternalIncomingEvent {
    Receive(Arc<Mutex<InternalClient>>),
}

enum InternalResult {
    Ok,
    Unhandled,
    Disconnected,
}

impl Server {
    pub fn launch_new() -> Result<(ServerIncoming, ServerStreaming)> {
        let rx_token = Token(0);

        let (server_tx, server_rx) = mio_extras::channel::channel::<InternalCommand>();

        let (incoming_tx, incoming_rx) = mio_extras::channel::channel::<InternalIncomingEvent>();
        let incoming_poll = Poll::new()?;
        incoming_poll.register(&incoming_rx, rx_token, Ready::readable(), PollOpt::edge())?;

        let (streaming_tx, streaming_rx) = mio_extras::channel::channel::<StreamingEvent>();
        let streaming_poll = Poll::new()?;
        streaming_poll.register(&streaming_rx, rx_token, Ready::readable(), PollOpt::edge())?;

        let server_tx = Arc::new(Mutex::new(server_tx));

        let server_tx1 = server_tx.clone();
        let server_tx2 = server_tx.clone();

        let result = (
            ServerIncoming {
                incoming_poll,
                incoming_rx,
                server_tx: server_tx1,
            },
            ServerStreaming {
                streaming_poll,
                streaming_rx,
                _server_tx: server_tx2,
            },
        );

        let mut thread = ServerThread {
            clients: HashMap::new(),
            server_tx: Arc::downgrade(&server_tx),
            server_rx,
            incoming_tx,
            streaming_tx,
            poll: Poll::new()?,
            tokens: (1..1024).map(Token).collect(),
            listeners: HashMap::new(),
            rx_token: Token(0),
        };

        let _join_handle = std::thread::spawn(move || {
            if let Err(e) = thread.run() {
                error!("thread finished with error: {}", e.description());
            }
        });

        Ok(result)
    }
}

impl ServerIncoming {
    pub fn add_listener(&self, listener: TcpListener) -> Result<()> {
        self.server_tx
            .lock()
            .unwrap()
            .send(InternalCommand::AddListener(listener))
            .unwrap();
        Ok(())
    }

    pub fn receive_next_fn<F, T>(&self, process: F) -> Result<IncomingResult<T>>
    where
        F: Fn(&[u8]) -> Receive<T>,
    {
        let mut events = Events::with_capacity(32);
        loop {
            self.incoming_poll.reregister(
                &self.incoming_rx,
                Token(0),
                Ready::readable(),
                PollOpt::edge(),
            )?;
            self.incoming_poll.poll(&mut events, None)?;

            let event = match self.incoming_rx.try_recv() {
                Ok(ev) => ev,
                Err(err) => match err {
                    TryRecvError::Empty => {
                        continue;
                    }
                    TryRecvError::Disconnected => {
                        return Ok(IncomingResult::Shutdown);
                    }
                },
            };

            match event {
                InternalIncomingEvent::Receive(internal_client) => {
                    let temp_client = internal_client.clone();
                    let client = temp_client.lock().unwrap();

                    if client.token.is_none() {
                        continue;
                    }

                    match process(&client.buffer) {
                        Receive::Receive(v) => {
                            return match self
                                .server_tx
                                .lock()
                                .unwrap()
                                .send(InternalCommand::Waiting(internal_client.clone()))
                            {
                                Ok(_) => Ok(IncomingResult::Request(
                                    IncomingClient {
                                        internal: internal_client.clone(),
                                    },
                                    v,
                                )),
                                Err(err) => match err {
                                    SendError::Io(err) => Err(err),
                                    SendError::Disconnected(_) => Ok(IncomingResult::Shutdown),
                                },
                            };
                        }
                        Receive::Invalid => {
                            if let Err(err) = self
                                .server_tx
                                .lock()
                                .unwrap()
                                .send(InternalCommand::Close(internal_client.clone()))
                            {
                                return match err {
                                    SendError::Io(err) => Err(err),
                                    SendError::Disconnected(_) => Ok(IncomingResult::Shutdown),
                                };
                            }
                        }
                        Receive::None => {}
                    }
                }
            }
        }
    }
}

impl IncomingClient {
    pub fn send(self, data: &[u8]) -> Result<()> {
        let internal_temp = self.internal.clone();
        let internal = internal_temp.lock().unwrap();
        if internal.token.is_some() {
            if let Err(SendError::Io(err)) = internal
                .server_tx
                .lock()
                .unwrap()
                .send(InternalCommand::Drain(self.internal, BytesMut::from(data)))
            {
                return Err(err);
            }
        }

        Ok(())
    }

    pub fn into_stream(self, out_buf: BytesMut) -> Result<StreamingClient> {
        let internal_temp = self.internal.clone();
        let internal = internal_temp.lock().unwrap();
        if internal.token.is_some() {
            if let Err(SendError::Io(err)) = internal
                .server_tx
                .lock()
                .unwrap()
                .send(InternalCommand::Stream(self.internal.clone(), out_buf))
            {
                return Err(err);
            }
        }

        Ok(StreamingClient {
            internal: self.internal,
        })
    }
}

impl ServerStreaming {
    pub fn streaming_next(&self) -> Result<StreamingEvent> {
        let mut events = Events::with_capacity(32);

        loop {
            self.streaming_poll.reregister(
                &self.streaming_rx,
                Token(0),
                Ready::readable(),
                PollOpt::edge(),
            )?;
            self.streaming_poll.poll(&mut events, None)?;

            let event = match self.streaming_rx.try_recv() {
                Ok(ev) => ev,
                Err(err) => match err {
                    TryRecvError::Empty => {
                        continue;
                    }
                    TryRecvError::Disconnected => {
                        return Ok(StreamingEvent::Shutdown);
                    }
                },
            };

            return Ok(event);
        }
    }
}

impl StreamingClient {
    pub fn status(&self) -> StreamingClientStatus {
        let internal = self.internal.lock().unwrap();

        if internal.token.is_none() {
            StreamingClientStatus::Closed
        } else if internal.buffer.is_empty() {
            StreamingClientStatus::Ready
        } else {
            StreamingClientStatus::Waiting
        }
    }

    pub fn feed(&self, data: &[u8]) {
        let internal = self.internal.lock().unwrap();

        if internal.token.is_some() {
            let _ = internal
                .server_tx
                .lock()
                .unwrap()
                .send(InternalCommand::StreamFeed(
                    self.internal.clone(),
                    BytesMut::from(data),
                ));
        }
    }

    pub fn drain(&self, data: &[u8]) {
        let internal = self.internal.lock().unwrap();

        if internal.token.is_some() {
            let _ = internal
                .server_tx
                .lock()
                .unwrap()
                .send(InternalCommand::StreamDrain(
                    self.internal.clone(),
                    BytesMut::from(data),
                ));
        }
    }
}

impl ServerThread {
    fn run(&mut self) -> Result<()> {
        debug!("started");

        self.poll.register(
            &self.server_rx,
            self.rx_token,
            Ready::readable(),
            PollOpt::edge(),
        )?;

        let mut events = Events::with_capacity(1024);

        'main: loop {
            self.poll.poll(&mut events, None)?;

            for event in events.iter() {
                let event_token = event.token();

                if event_token == self.rx_token {
                    match self.process_server_rx()? {
                        InternalResult::Ok => {
                            continue;
                        }
                        InternalResult::Unhandled => {
                            unreachable!();
                        }
                        InternalResult::Disconnected => {
                            break 'main;
                        }
                    }
                }

                match self.try_process_listener(&event)? {
                    InternalResult::Ok => continue,
                    InternalResult::Unhandled => {}
                    InternalResult::Disconnected => {
                        break 'main;
                    }
                }

                match self.try_process_client(&event)? {
                    InternalResult::Ok => {}
                    InternalResult::Unhandled => {
                        error!("token without matching client from poll");
                        continue;
                    }
                    InternalResult::Disconnected => {
                        break 'main;
                    }
                }
            }
        }

        debug!("stopping");

        Ok(())
    }

    fn process_server_rx(&mut self) -> Result<InternalResult> {
        let command = match self.server_rx.try_recv() {
            Ok(c) => c,
            Err(err) => {
                return Ok(match err {
                    TryRecvError::Empty => InternalResult::Ok,
                    TryRecvError::Disconnected => InternalResult::Disconnected,
                });
            }
        };

        self.poll.reregister(
            &self.server_rx,
            self.rx_token,
            Ready::readable(),
            PollOpt::edge(),
        )?;

        match command {
            InternalCommand::AddListener(listener) => match self.tokens.pop() {
                Some(token) => {
                    self.poll
                        .register(&listener, token, Ready::readable(), PollOpt::edge())?;
                    self.listeners
                        .insert(token, InternalListener { token, listener });
                }
                None => {
                    error!("max connections reached");
                }
            },
            InternalCommand::Close(client) => {
                let mut client = client.lock().unwrap();
                if let Some(token) = client.token.take() {
                    self.poll.deregister(&client.stream)?;
                    self.clients.remove(&token);
                    self.tokens.push(token);
                }
            }
            InternalCommand::Waiting(client) => {
                let mut client = client.lock().unwrap();
                if let Some(token) = client.token {
                    self.poll
                        .reregister(&client.stream, token, Ready::empty(), PollOpt::edge())?;
                    client.state = InternalClientState::Waiting;
                }
            }
            InternalCommand::Drain(client, buffer) => {
                let mut client = client.lock().unwrap();
                if let Some(token) = client.token {
                    self.poll.reregister(
                        &client.stream,
                        token,
                        Ready::writable(),
                        PollOpt::edge(),
                    )?;
                    client.state = InternalClientState::Drain;
                    client.buffer = buffer;
                }
            }
            InternalCommand::Stream(client, buffer) => {
                let mut client = client.lock().unwrap();
                if let Some(token) = client.token {
                    self.poll.reregister(
                        &client.stream,
                        token,
                        Ready::writable(),
                        PollOpt::edge(),
                    )?;
                    client.state = InternalClientState::Streaming;
                    client.buffer = buffer;
                }
            }
            InternalCommand::StreamFeed(client, buffer) => {
                let mut client = client.lock().unwrap();
                if let Some(token) = client.token {
                    self.poll.reregister(
                        &client.stream,
                        token,
                        Ready::writable(),
                        PollOpt::edge(),
                    )?;
                    client.buffer.extend_from_slice(&buffer);
                }
            }
            InternalCommand::StreamDrain(client, buffer) => {
                let mut client = client.lock().unwrap();
                if let Some(token) = client.token {
                    self.poll.reregister(
                        &client.stream,
                        token,
                        Ready::writable(),
                        PollOpt::edge(),
                    )?;
                    client.state = InternalClientState::Drain;
                    client.buffer.extend_from_slice(&buffer);
                }
            }
            InternalCommand::Shutdown => {
                return Ok(InternalResult::Disconnected);
            }
        }

        Ok(InternalResult::Ok)
    }

    fn try_process_listener(&mut self, event: &Event) -> Result<InternalResult> {
        let event_token = event.token();

        if let Some(listener) = self.listeners.get_mut(&event_token) {
            let (stream, address) = match listener.listener.accept() {
                Ok(c) => c,
                Err(e) => {
                    error!("tcp accept error: {}", e.description());
                    return Ok(InternalResult::Ok);
                }
            };

            self.poll.reregister(
                &listener.listener,
                listener.token,
                Ready::readable(),
                PollOpt::edge(),
            )?;

            let token = match self.tokens.pop() {
                Some(t) => t,
                None => {
                    error!("max connections reached");
                    return Ok(InternalResult::Ok);
                }
            };

            self.poll
                .register(&stream, token, Ready::readable(), PollOpt::edge())?;

            trace!("accepted client from {}", address);

            self.clients.insert(
                token,
                Arc::new(Mutex::new(InternalClient {
                    token: Some(token),
                    state: InternalClientState::Incoming,
                    stream,
                    buffer: BytesMut::new(),
                    server_tx: match self.server_tx.upgrade() {
                        Some(tx) => tx,
                        None => {
                            return Ok(InternalResult::Disconnected);
                        }
                    },
                })),
            );

            Ok(InternalResult::Ok)
        } else {
            Ok(InternalResult::Unhandled)
        }
    }

    fn try_process_client(&mut self, event: &Event) -> Result<InternalResult> {
        let event_token = event.token();

        let mutex_client = match self.clients.get_mut(&event_token) {
            Some(c) => c,
            None => {
                return Ok(InternalResult::Unhandled);
            }
        };

        let temp_client = mutex_client.clone();
        let mut client = temp_client.lock().unwrap();

        match client.state {
            InternalClientState::Incoming => {
                if event.readiness().is_readable() {
                    let mut buf = [0; 1024];
                    let n = match client.stream.read(&mut buf) {
                        Ok(n) => n,
                        Err(e) => {
                            error!("tcp read error: {}", e.description());

                            self.poll.deregister(&client.stream)?;
                            let token = client.token.take().unwrap();
                            self.clients.remove(&token);
                            self.tokens.push(token);

                            return Ok(InternalResult::Ok);
                        }
                    };

                    client.buffer.extend_from_slice(&buf[0..n]);

                    if let Err(SendError::Disconnected(_)) = self
                        .incoming_tx
                        .send(InternalIncomingEvent::Receive(mutex_client.clone()))
                    {
                        return Ok(InternalResult::Disconnected);
                    }
                }

                self.poll.reregister(
                    &client.stream,
                    client.token.unwrap(),
                    Ready::readable(),
                    PollOpt::edge(),
                )?;
            }
            InternalClientState::Waiting => {
                self.poll.reregister(
                    &client.stream,
                    client.token.unwrap(),
                    Ready::empty(),
                    PollOpt::edge(),
                )?;
            }
            InternalClientState::Drain | InternalClientState::Streaming => {
                if event.readiness().is_writable() {
                    let buf = client.buffer.clone();
                    let n = match client.stream.write(&buf) {
                        Ok(n) => n,
                        Err(e) => {
                            error!("tcp write error: {}", e.description());

                            self.poll.deregister(&client.stream)?;
                            let token = client.token.take().unwrap();
                            self.clients.remove(&token);
                            self.tokens.push(token);

                            if let InternalClientState::Streaming = client.state {
                                if let Err(SendError::Disconnected(_)) =
                                    self.streaming_tx.send(StreamingEvent::Event)
                                {
                                    return Ok(InternalResult::Disconnected);
                                }
                            }

                            return Ok(InternalResult::Ok);
                        }
                    };

                    client.buffer.advance(n);
                }

                if client.buffer.is_empty() {
                    match client.state {
                        InternalClientState::Drain => {
                            self.poll.deregister(&client.stream)?;
                            let token = client.token.take().unwrap();
                            self.clients.remove(&token);
                            self.tokens.push(token);
                        }
                        InternalClientState::Streaming => {
                            self.poll.reregister(
                                &client.stream,
                                client.token.unwrap(),
                                Ready::empty(),
                                PollOpt::edge(),
                            )?;

                            if let Err(SendError::Disconnected(_)) =
                                self.streaming_tx.send(StreamingEvent::Event)
                            {
                                return Ok(InternalResult::Disconnected);
                            }
                        }
                        _ => unreachable!(),
                    }
                } else {
                    self.poll.reregister(
                        &client.stream,
                        client.token.unwrap(),
                        Ready::writable(),
                        PollOpt::edge(),
                    )?;
                }
            }
        }

        Ok(InternalResult::Ok)
    }
}
