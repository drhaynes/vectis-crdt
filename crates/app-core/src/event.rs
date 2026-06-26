use crate::network::Direction;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppEvent {
    PacketCreated {
        id: u32,
        direction: Direction,
        bytes: usize,
    },
    PacketDelivered {
        id: u32,
    },
    ClearPackets,
}
