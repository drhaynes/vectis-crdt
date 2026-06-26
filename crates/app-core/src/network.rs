#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Outbound,
    Inbound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionState {
    pub connected: bool,
    pub loaded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireEntry {
    pub direction: Direction,
    pub kind: &'static str,
    pub bytes: usize,
    pub hex: String,
}

pub(crate) const MAX_LOG: usize = 12;

pub(crate) fn hex_prefix(payload: &[u8]) -> String {
    payload
        .iter()
        .take(8)
        .map(|byte| format!("{:02x}", byte))
        .collect::<Vec<_>>()
        .join(" ")
}
