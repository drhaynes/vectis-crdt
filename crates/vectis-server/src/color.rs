use vectis_crdt::types::ActorId;

pub(crate) fn actor_color(actor: ActorId) -> u32 {
    const COLORS: [u32; 8] = [
        0xa78bfaff, 0x60a5fbff, 0x34d399ff, 0xfbbf24ff, 0xf87171ff, 0x22d3eeff, 0xfb7185ff,
        0xc084fcff,
    ];
    COLORS[(actor.0 as usize - 1) % COLORS.len()]
}
