//! Client-side networking

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetEvent {
    /// A message from the peer, or a pairing event (`PeerJoined`, `PeerLeft`, `Room`, `Error`) from the relay or from the 
    /// local listener
    Msg(Msg),
    /// Connection gode: nothing more will arrive
    Closed(String),
}