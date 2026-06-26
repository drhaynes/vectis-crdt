pub const ALICE_ACTOR: u64 = 1;
pub const BOB_ACTOR: u64 = 2;
pub const ALICE_COLOR: u32 = 0xa78bfaff;
pub const BOB_COLOR: u32 = 0x60a5fbff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    Alice,
    Bob,
}
