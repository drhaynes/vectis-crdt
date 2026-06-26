#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientEvent {
    SendFrame(Vec<u8>),
}
