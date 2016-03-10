use std::io;
use std::io::prelude::*;
use std::io::{Error, ErrorKind};
use std::rc::Rc;

use byteorder::{ByteOrder, BigEndian, LittleEndian};

use mio::*;
use mio::tcp::*;

use server::Server;

#[derive(Clone)]
pub enum MessageType{
    Login { name_len: u32 },
    Logout,
    Message { name_len: u32, msg_len: u32 },
    List
}

struct MessageHeader{
    message_length: u64,
    message_data: Option<MessageType>
}

impl MessageHeader{
    fn new(length: u64, message: Option<MessageType>) -> MessageHeader{
        MessageHeader{ message_length: length, message_data: message }
    }
}

#[derive(Clone)]
pub struct Message{
    pub username: String,
    pub msg_type: MessageType,
    text: Option<String>
}

impl Message{
    pub fn new(name: String, msg_type: MessageType, message: Option<String>) -> Message{
        Message{ username: name, msg_type: msg_type, text: message }
    }

    pub fn get_message(&self) -> String{
        match self.msg_type{
            MessageType::Login{name_len:_} => {
                let prefix = "<<<";
                let suffix = " has joined the room>>>";

                let mut message = String::with_capacity(prefix.len() + suffix.len() + self.username.len());
                message.push_str(prefix);
                message.push_str(self.username.as_str());
                message.push_str(suffix);
                return message.clone();
            },
            MessageType::Logout => {
                let prefix = "<<<";
                let suffix = " has left the room>>>";

                let mut message = String::with_capacity(prefix.len() + suffix.len() + self.username.len());
                message.push_str(prefix);
                message.push_str(self.username.as_str());
                message.push_str(suffix);
                return message.clone();
            },
            MessageType::Message{name_len: _, msg_len: len} => {
                let separator = ": ";

                let mut message = String::with_capacity(len as usize);
                message.push_str(self.username.as_str());
                message.push_str(separator);
                let msg_text = self.text.clone().unwrap();
                message.push_str(msg_text.as_str().clone());
                return message.clone();
            },
            MessageType::List => {
                let message = self.text.clone().unwrap_or_else(|| String::new());
                return message.clone();
            }
        }
    }

    pub fn get_send_message(&self) -> String{
        let msg_text = self.get_message();
        let mut msg_to_send = String::with_capacity(self.username.len() as usize + msg_text.len() as usize);
        msg_to_send.push_str(self.username.as_str());
        msg_to_send.push_str(msg_text.as_str());
        return msg_to_send;
    }

    pub fn is_list(&self) -> bool{
        match self.msg_type{
            MessageType::List => true,
            _ => false
        }
    }
}
/// A stateful wrapper around a non-blocking stream. This connection is not
/// the SERVER connection. This connection represents the client connections
/// _accepted_ by the SERVER connection.
pub struct Connection {
    // handle to the accepted socket
    sock: TcpStream,

    // token used to register with the event loop
    pub token: Token,

    // set of events we are interested in
    interest: EventSet,

    // messages waiting to be sent out
    send_queue: Vec<Rc<Message>>,

    // track whether a connection needs to be (re)registered
    is_idle: bool,

    // track whether a connection is reset
    is_reset: bool,

    // track whether a read received `WouldBlock` and store the number of
    // byte we are supposed to read
    read_continuation: Option<u64>,

    // track whether a write received `WouldBlock`
    write_continuation: bool,

    // The name of the user associated with this connection
    pub username: String

}

impl Connection {
    pub fn new(sock: TcpStream, token: Token) -> Connection {
        Connection {
            sock: sock,
            token: token,
            interest: EventSet::hup(),
            send_queue: Vec::new(),
            is_idle: true,
            is_reset: false,
            read_continuation: None,
            write_continuation: false,
            username: String::new()
        }
    }

    /// Handle read event from event loop.
    ///
    /// The Handler must continue calling until None is returned.
    ///
    /// The recieve buffer is sent back to `Server` so the message can be broadcast to all
    /// listening connections.
    pub fn readable(&mut self) -> io::Result<Option<Message>> {

        // let msg_len = match try!(self.read_message_length()) {
        //     None => { return Ok(None); },
        //     Some(n) => n,
        // };

        let message_info: MessageHeader = match try!(self.read_message_info()){
            None => { return Ok(None); },
            Some(n) => n,
        };

        let msg_len = match message_info.message_data {
            Some(MessageType::Login{ name_len })  => { name_len },
            Some(MessageType::Logout) => { 0 },
            Some(MessageType::Message{ name_len, msg_len: mesg_len }) => { name_len + mesg_len },
            Some(MessageType::List) => { 0 },
            None => {
                error!("Received invalid message type)!");
                //@TODO ************
                // PROBABLY NOT ACTUALLY AN ERROR CASE DONT RETURN HERE
                return Err(Error::new(ErrorKind::InvalidData, "Invalid message type"));
            }
        };

        // if msg_len == 0 {
        //     debug!("message is zero bytes; token={:?}", self.token);
        //     return Ok(None);
        // }

        debug!("Expected message length: {}", msg_len);
        let mut recv_buf : Vec<u8> = Vec::with_capacity(msg_len as usize);

        // UFCS: resolve "multiple applicable items in scope [E0034]" error
        let sock_ref = <TcpStream as Read>::by_ref(&mut self.sock);

        match sock_ref.take(msg_len as u64).try_read_buf(&mut recv_buf) {
            Ok(None) => {
                debug!("CONN : read encountered WouldBlock");

                // We are being forced to try again, but we already read the two bytes off of the
                // wire that determined the length. We need to store the message length so we can
                // resume next time we get readable.
                self.read_continuation = Some(msg_len as u64);
                Ok(None)
            },
            Ok(Some(n)) => {
                debug!("CONN : we read {} bytes", n);

                debug!("Data: {:?}", recv_buf);

                if n < msg_len as usize {
                    return Err(Error::new(ErrorKind::InvalidData, "Did not read enough bytes"));
                }

                self.read_continuation = None;

                {
                    match message_info.message_data{
                        Some(MessageType::Login{ name_len }) => {
                            recv_buf.drain(name_len as usize..);
                            {
                                let ref username = recv_buf;
                                self.username = String::from_utf8(username.clone()).unwrap();
                            }
                            Ok(Some(Message::new(self.username.clone(), MessageType::Login{name_len: self.username.len() as u32}, None)))
                        },
                        Some(MessageType::Logout) => {
                            Ok(Some(Message::new(self.username.clone(), MessageType::Logout, None)))
                        },
                        Some(MessageType::Message{ name_len, msg_len }) => {
                            let message_text: Vec<u8> = recv_buf.drain(name_len as usize..).collect();
                            {
                                let ref username = recv_buf;
                                self.username = String::from_utf8(username.clone()).unwrap();
                            }
                            Ok(Some(Message::new(self.username.clone(), MessageType::Message{name_len: self.username.len() as u32, msg_len: message_text.len() as u32}, Some(String::from_utf8(message_text).unwrap()))))
                        },
                        Some(MessageType::List) => {
                            Ok(Some(Message::new(self.username.clone(), MessageType::List, None)))
                        },
                        None => {Ok(None)}
                    }
                }
                //let message = Message::new(self.username.clone(), String::from_utf8(message_text).unwrap());
                //println!("{}: {}", message.username, message.message);

                //Ok(Some(recv_buf))
            },
            Err(e) => {
                error!("Failed to read buffer for token {:?}, error: {}", self.token, e);
                Err(e)
            }
        }
    }

    fn read_message_info(&mut self) -> io::Result<Option<MessageHeader>>{
        if let Some(n) = self.read_continuation {
            return Ok(Some(MessageHeader{ message_length: n, message_data: None}));
        }

        let mut buf = [0u8; 12];

        let bytes = match self.sock.try_read(&mut buf){
            Ok(None) =>{
                return Ok(None);
            },
            Ok(Some(n)) => n,
            Err(e) => {
                return Err(e);
            }
        };

        if bytes < 5{
            warn!("Found message length of {} bytes", bytes);
            return Err(Error::new(ErrorKind::InvalidData, "Invalid message length"));
        }

        debug!("Received: {:?}", buf);

        let command = LittleEndian::read_u32(buf[0..4].as_ref());
        debug!("Command number is {}", command);

        let name_len = LittleEndian::read_u32(buf[4..8].as_ref());
        debug!("Name length is {}", name_len);

        let msg_len = LittleEndian::read_u32(buf[8..12].as_ref());
        debug!("message length is {}", msg_len);

        let data_len = (name_len + msg_len) as u64;

        let message: MessageType = match command{
            0 => { MessageType::Login{ name_len: name_len } },
            1 => { MessageType::Logout },
            2 => { MessageType::Message{ name_len: name_len, msg_len: msg_len } },
            3 => { MessageType::List },
            n => {
                warn!("Received invalid message type, {}!", n);
                return Err(Error::new(ErrorKind::InvalidData, "Invalid message type received"));
            }
        };

        return Ok(Some(MessageHeader::new(data_len, Some(message))));
    }

    fn read_message_length(&mut self) -> io::Result<Option<u64>> {
        if let Some(n) = self.read_continuation {
            return Ok(Some(n));
        }

        let mut buf = [0u8; 12];

        let bytes = match self.sock.try_read(&mut buf) {
            Ok(None) => {
                return Ok(None);
            },
            Ok(Some(n)) => n,
            Err(e) => {
                return Err(e);
            }
        };

        if bytes < 12 {
            warn!("Found message length of {} bytes", bytes);
            return Err(Error::new(ErrorKind::InvalidData, "Invalid message length"));
        }

        debug!("Received: {:?}", buf);

        let command = LittleEndian::read_i32(buf[0..4].as_ref());
        debug!("Command number is {}", command);

        let name_len = LittleEndian::read_i32(buf[4..8].as_ref());
        debug!("Name length is {}", name_len);

        let msg_len = LittleEndian::read_i32(buf[8..12].as_ref());
        debug!("message length is {}", msg_len);

        let msg_len = (12 + name_len + msg_len) as u64;
        Ok(Some(msg_len))
    }

    /// Handle a writable event from the event loop.
    ///
    /// Send one message from the send queue to the client. If the queue is empty, remove interest
    /// in write events.
    /// TODO: Figure out if sending more than one message is optimal. Maybe we should be trying to
    /// flush until the kernel sends back EAGAIN?
    pub fn writable(&mut self) -> io::Result<()> {
        if self.send_queue.is_empty() {
            return Ok(());
        }

        try!(self.send_queue.pop()
            .ok_or(Error::new(ErrorKind::Other, "Could not pop send queue"))
            .and_then(|msg| {
                // match self.write_message_length(&msg) {
                //     Ok(None) => {
                //         // put message back into the queue so we can try again
                //         self.send_queue.push(msg);
                //         return Ok(());
                //     },
                //     Ok(Some(())) => {
                //         ()
                //     },
                //     Err(e) => {
                //         error!("Failed to send buffer for {:?}, error: {}", self.token, e);
                //         return Err(e);
                //     }
                // }

                let msg_type = match(msg.msg_type){
                    MessageType::Login{ name_len: _} => 0,
                    MessageType::Logout => 1,
                    MessageType::Message{ name_len: _, msg_len: _} => 2,
                    MessageType::List => 3
                };

                let buf = msg.get_message();
                let len = buf.as_bytes().len();
                let mut send_buf = [0u8; 12];
                LittleEndian::write_i32(&mut send_buf[0..4], msg_type as i32);
                LittleEndian::write_i32(&mut send_buf[4..8], msg.username.len() as i32);
                LittleEndian::write_i32(&mut send_buf[8..12], len as i32);

                let msg_bytes = msg.get_send_message();
                let mut bytes_to_send : Vec<u8> = Vec::with_capacity(msg_bytes.len() + send_buf.len());
                bytes_to_send.extend_from_slice(send_buf.as_ref());
                bytes_to_send.extend_from_slice(msg_bytes.as_bytes());

                let send_bytes = bytes_to_send.as_slice();


                match self.sock.try_write(&*send_bytes) {
                    Ok(None) => {
                        debug!("client flushing buf; WouldBlock");

                        // put message back into the queue so we can try again
                        self.send_queue.push(msg);
                        self.write_continuation = true;
                        Ok(())
                    },
                    Ok(Some(n)) => {
                        debug!("CONN : we wrote {} bytes", n);
                        self.write_continuation = false;
                        Ok(())
                    },
                    Err(e) => {
                        error!("Failed to send buffer for {:?}, error: {}", self.token, e);
                        Err(e)
                    }
                }
            })
        );

        if self.send_queue.is_empty() {
            self.interest.remove(EventSet::writable());
        }

        Ok(())
    }

    fn write_message_length(&mut self, msg: &Rc<Message>) -> io::Result<Option<()>> {
        if self.write_continuation {
            return Ok(Some(()));
        }

        let msg_type = match(msg.msg_type){
            MessageType::Login{ name_len: _} => 0,
            MessageType::Logout => 1,
            MessageType::Message{ name_len: _, msg_len: _} => 2,
            MessageType::List => 3
        };

        let buf = msg.get_message();
        let len = buf.as_bytes().len();
        let mut send_buf = [0u8; 12];
        LittleEndian::write_i32(&mut send_buf[0..4], msg_type as i32);
        LittleEndian::write_i32(&mut send_buf[4..8], msg.username.len() as i32);
        LittleEndian::write_i32(&mut send_buf[8..12], len as i32);

        match self.sock.try_write(&send_buf) {
            Ok(None) => {
                debug!("client flushing buf; WouldBlock");

                Ok(None)
            },
            Ok(Some(n)) => {
                debug!("Sent message length of {} bytes", n);
                Ok(Some(()))
            },
            Err(e) => {
                error!("Failed to send buffer for {:?}, error: {}", self.token, e);
                Err(e)
            }
        }
    }

    /// Queue an outgoing message to the client.
    ///
    /// This will cause the connection to register interests in write events with the event loop.
    /// The connection can still safely have an interest in read events. The read and write buffers
    /// operate independently of each other.
    pub fn send_message(&mut self, message: Rc<Message>) -> io::Result<()> {
        trace!("connection send_message; token={:?}", self.token);

        self.send_queue.push(message);

        if !self.interest.is_writable() {
            self.interest.insert(EventSet::writable());
        }

        Ok(())
    }

    /// Register interest in read events with the event_loop.
    ///
    /// This will let our connection accept reads starting next event loop tick.
    pub fn register(&mut self, event_loop: &mut EventLoop<Server>) -> io::Result<()> {
        trace!("connection register; token={:?}", self.token);

        self.interest.insert(EventSet::readable());

        event_loop.register(
            &self.sock,
            self.token,
            self.interest,
            PollOpt::edge() | PollOpt::oneshot()
        ).and_then(|(),| {
            self.is_idle = false;
            Ok(())
        }).or_else(|e| {
            error!("Failed to reregister {:?}, {:?}", self.token, e);
            Err(e)
        })
    }

    /// Re-register interest in read events with the event_loop.
    pub fn reregister(&mut self, event_loop: &mut EventLoop<Server>) -> io::Result<()> {
        trace!("connection reregister; token={:?}", self.token);

        event_loop.reregister(
            &self.sock,
            self.token,
            self.interest,
            PollOpt::edge() | PollOpt::oneshot()
        ).and_then(|(),| {
            self.is_idle = false;
            Ok(())
        }).or_else(|e| {
            error!("Failed to reregister {:?}, {:?}", self.token, e);
            Err(e)
        })
    }

    //pub fn deregister(&mut self, event_loop: &mut EventLoop<Server>) -> io::Result<()> {
    //    trace!("connection deregister; token={:?}", self.token);

    //    event_loop.deregister(
    //        &self.sock
    //    ).or_else(|e| {
    //        error!("Failed to deregister {:?}, {:?}", self.token, e);
    //        Err(e)
    //    })
    //}

    pub fn mark_reset(&mut self) {
        trace!("connection mark_reset; token={:?}", self.token);

        self.is_reset = true;
    }

    #[inline]
    pub fn is_reset(&self) -> bool {
        self.is_reset
    }

    pub fn mark_idle(&mut self) {
        trace!("connection mark_idle; token={:?}", self.token);

        self.is_idle = true;
    }

    #[inline]
    pub fn is_idle(&self) -> bool {
        self.is_idle
    }

    pub fn has_write_queued(&self) -> bool{
        self.interest.is_writable()
    }
}
