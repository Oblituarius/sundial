//! The source build's platform, for readers that take a payload alone.
//!
//! Rise of Iron's PS4 build packs structures the Xbox One build pads: every relative pointer,
//! name hash and trailing word is 24 bytes on Xbox One and 16 on PS4, so the fields after such
//! a group sit earlier. It also numbers its script class tags (`80AAxxxx`) on its own. Readers
//! written against the Xbox One build name each field or class for both builds through these
//! helpers. [`Source::open`](super::Source::open) records the build of the packages it reads.
use std::sync::atomic::{AtomicBool, Ordering};

static PS4: AtomicBool = AtomicBool::new(false);

pub(super) fn set(platform: u16) {
    PS4.store(platform == 7, Ordering::Relaxed);
}

pub(crate) fn ps4() -> bool {
    PS4.load(Ordering::Relaxed)
}

/// A field offset, row stride or count by build: Xbox One, then PS4.
pub(crate) fn field(xbox: usize, ps4: usize) -> usize {
    if self::ps4() { ps4 } else { xbox }
}

/// An effect sequence definition (`80800610`) field, given by its Xbox One offset. Ten packed
/// groups precede the node tables at `+160` and three more, from `+248` on, precede the auxiliary
/// inputs at `+2A8`, so PS4 keeps the tables at `+110` and the auxiliary inputs at `+240`.
pub(crate) fn sequence(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0..0x68 => xbox,
        // Nine packed declarations, a class word, a tenth declaration and a class word.
        0x78..0x138 if (xbox - 0x60) % 24 == 0 => 0x60 + (xbox - 0x60) / 24 * 16,
        0x138 => 0xF0,
        0x140 => 0xF8,
        0x158 => 0x108,
        0x160..=0x248 => xbox - 0x50,
        // The embedded owner after the callback declaration and its own two declarations.
        0x260 => 0x208,
        0x278 => 0x220,
        0x290 => 0x230,
        0x2A8.. => xbox - 0x68,
        _ => UNMEASURED,
    }
}

/// The stride of a scalar input definition row (`80800927`) and its name's offset in the row.
pub(crate) fn input_row() -> (usize, usize) {
    (field(48, 40), field(0x28, 0x20))
}

/// An offset no PS4 structure holds. A field with no measured PS4 position reads past its
/// payload and refuses its talent, rather than reading the Xbox One position's other field.
const UNMEASURED: usize = usize::MAX / 4;

/// A script tag (`80AAxxxx`) by build. Both builds list their generated tags in the same order
/// with different counts between them, so each measured run of Xbox One tags has one PS4
/// distance. A tag outside the measured runs names nothing on PS4.
pub(crate) fn tag(xbox: u32) -> u32 {
    if !ps4() {
        return xbox;
    }
    // Shared content owners the readers name: the invisibility presentation sequence, the
    // overshield sequence, the shared Duration host (the one host with key `BD15623E`) and
    // Virulence's swarm graph. Each was found on PS4 by the role the reader gives it.
    match xbox {
        0x80A7_E1DF => return 0x80A7_E344,
        0x80A7_E1E8 => return 0x80A7_E34C,
        0x80A7_F676 => return 0x80A7_F739,
        0x80A8_8D5B => return 0x80A8_8F18,
        // The external Duration publisher (HUD identity `D491B5AC`, priority 99) and the two
        // graphs that hold it.
        0x80A7_52C8 => return 0x80A7_52CC,
        0x80A6_B0F3 => return 0x80A6_B0A6,
        0x80B0_0C2F => return 0x80B0_0AD9,
        _ => {}
    }
    // Other content tags pass through. They are not all the same in both builds, so a reader
    // that names one has to be checked against PS4 data before it is trusted there.
    if !(0x80AA_0000..0x80AC_0000).contains(&xbox) {
        return xbox;
    }
    // In order: the shared invisibility owner; status, Duration, Timer and HUD parameter
    // declarations with the damage impulse classes; more of those declarations; the shared
    // network owner with its classes; the shared category owner; the channel bank declarations
    // up to the shared effect context owner; the nested controller's declaration; the Timer's
    // and Duration's later declarations.
    let distance = match xbox {
        0x80AA_2EA8 => 0xE,
        // The damage label dictionary.
        0x80AA_C26E => 0x914,
        0x80AA_C281..=0x80AA_C28C => 0x917,
        0x80AA_C444..=0x80AA_C44C => 0xA07,
        0x80AA_CEA6..=0x80AA_CEA8 => 0xCD7,
        0x80AA_D0F9 => 0xD47,
        0x80AA_D523..=0x80AA_D54B => 0xE59,
        0x80AA_E896 => 0x10FB,
        0x80AA_EDB3..=0x80AA_EDB4 => 0x112C,
        _ => return 0,
    };
    xbox + distance
}

/// A parameter declaration's script tag and trailing word. Xbox One stores a relative pointer,
/// the tag and the word in eight bytes each. PS4 packs the tag and the word into one.
pub(crate) fn declared(
    p: &crate::tiger::payload::Payload,
    at: usize,
) -> anyhow::Result<(u64, u64)> {
    if ps4() {
        Ok((u64::from(p.u32(at + 8)?), u64::from(p.u32(at + 12)?)))
    } else {
        Ok((p.u64(at + 8)?, p.u64(at + 16)?))
    }
}

/// Offsets within a status host (`80802698`) and its embedded HUD (`8080254F`), Duration
/// (`8080223C`) and Timer (`80802890`) records, each given by its Xbox One offset. Every packed
/// declaration before a field moves it eight bytes earlier on PS4, measured on all 499 PS4
/// status hosts. The runtime records keep their Xbox One layout.
pub(crate) mod status {
    use super::{UNMEASURED, ps4};

    /// Host fields. The receiver policy at `+D0` and its class at `+D8` are one packed pair at
    /// `+B8` on PS4, read through [`receiver`].
    pub(crate) fn settings(xbox: usize) -> usize {
        if !ps4() {
            return xbox;
        }
        match xbox {
            0..0x68 => xbox,
            0x78 => 0x70,
            0x90 => 0x80,
            0xA8..=0xCF => xbox - 0x18,
            0xE0..=0xF0 => xbox - 0x20,
            0x418 => 0x388,
            0x438 => 0x3A0,
            _ => UNMEASURED,
        }
    }

    /// The lifetime-sequence receiver's policy and class.
    pub(crate) fn receiver(
        p: &crate::tiger::payload::Payload,
        settings: usize,
    ) -> anyhow::Result<(u64, u64)> {
        if ps4() {
            Ok((
                u64::from(p.u32(settings + 0xB8)?),
                u64::from(p.u32(settings + 0xBC)?),
            ))
        } else {
            Ok((p.u64(settings + 0xD0)?, p.u64(settings + 0xD8)?))
        }
    }

    pub(crate) fn hud(xbox: usize) -> usize {
        if !ps4() {
            return xbox;
        }
        match xbox {
            0..0x20 => xbox,
            0x30 => 0x28,
            0x38 => 0x30,
            0x50 => 0x40,
            0x58 => 0x48,
            0x70 => 0x58,
            0x88..=0x160 => xbox - 0x20,
            _ => UNMEASURED,
        }
    }

    pub(crate) fn duration(xbox: usize) -> usize {
        if !ps4() {
            return xbox;
        }
        match xbox {
            0..0x20 => xbox,
            0x30 => 0x28,
            0x38 => 0x30,
            0x50 => 0x40,
            0x58 => 0x48,
            0x70 => 0x58,
            0x88 => 0x68,
            0xA0..=0xB8 => xbox - 0x28,
            0x138 => 0xF0,
            _ => UNMEASURED,
        }
    }

    /// Timer fields. The lifetime expression follows at `+80`.
    pub(crate) fn timer(xbox: usize) -> usize {
        if !ps4() {
            return xbox;
        }
        match xbox {
            0..0x20 => xbox,
            0x30 => 0x28,
            0x48 => 0x38,
            0x60 => 0x48,
            0x78..=0x80 => xbox - 0x20,
            _ => UNMEASURED,
        }
    }
}

/// A channel bank definition (`808008F6`) field, given by its Xbox One offset. Its four packed
/// declarations at `+60` put the declaration rows at `+B8` on PS4, not `+D8`.
pub(crate) fn bank(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0..0x68 => xbox,
        0x78 => 0x70,
        0x90 => 0x80,
        0xA8 => 0x90,
        0xC0.. => xbox - 0x20,
        _ => UNMEASURED,
    }
}

/// A sequence node's kind. Xbox One (MSVC) pads the kind byte at `+1C` to four bytes and starts
/// the event's own fields at `+20`. PS4 (clang) puts an event's leading byte fields in that
/// padding, at `+1D`, so only the first byte is the kind there.
pub(crate) fn event_kind(p: &crate::tiger::payload::Payload, at: usize) -> anyhow::Result<u32> {
    if ps4() {
        Ok(u32::from(p.u8(at + 0x1C)?))
    } else {
        p.u32(at + 0x1C)
    }
}

/// A receiver definition's (`808005A8`) row table offset, and its rows' (`80800884`) stride,
/// name offset and four-lane value offset. Each PS4 row packs its declaration, which puts the
/// name at `+28` and the lanes at `+30`.
pub(crate) fn receiver_rows() -> (usize, usize, usize, usize) {
    (
        field(0x90, 0x80),
        field(0x50, 0x40),
        field(0x30, 0x28),
        field(0x40, 0x30),
    )
}
