use vectis_crdt::types::ActorId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolMessage {
    ClientHello {
        room: String,
        resume_token: String,
        state_vector: Vec<u8>,
    },
    ServerWelcome {
        actor: ActorId,
        color: u32,
        resume_token: String,
    },
    Snapshot {
        bytes: Vec<u8>,
    },
    Update {
        bytes: Vec<u8>,
    },
    StateVector {
        bytes: Vec<u8>,
    },
    Mvv {
        bytes: Vec<u8>,
    },
    Awareness {
        bytes: Vec<u8>,
    },
    Error {
        message: String,
    },
}
