//! The source build's platform, for readers that take a payload alone.
//!
//! Rise of Iron's PS4 build packs structures the Xbox One build pads: every relative pointer,
//! name hash and trailing word is 24 bytes on Xbox One and 16 on PS4, so the fields after such
//! a group sit earlier. It also numbers its script class tags (`80AAxxxx`) on its own. Readers
//! written against the Xbox One build name each field or class for both builds through these
//! helpers. [`Source::open`](super::Source::open) records the build of the packages it reads.
use std::{
    collections::BTreeSet,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

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
        _ => unmeasured("sequence", xbox),
    }
}

/// The stride of a scalar input definition row (`80800927`) and its name's offset in the row.
pub(crate) fn input_row() -> (usize, usize) {
    (field(48, 40), field(0x28, 0x20))
}

/// An offset no PS4 structure holds. A field with no measured PS4 position reads past its
/// payload and refuses its talent, rather than reading the Xbox One position's other field.
const UNMEASURED: usize = usize::MAX / 4;

/// PS4 lookups that fell outside the measured tables since the last [`take_misses`]: helper and
/// Xbox One offset, or an Xbox One tag with no PS4 counterpart. The talent census reports them
/// with each step, so a refusal caused by an unported layout names the field it lacks. Only PS4
/// lookups record anything; a set keeps a long import from growing it.
static MISSES: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn miss(text: String) {
    if let Ok(mut misses) = MISSES.lock()
        && misses.len() < 64
    {
        misses.insert(text);
    }
}

fn unmeasured(helper: &str, xbox: usize) -> usize {
    miss(format!("{helper} {xbox:#X}"));
    UNMEASURED
}

fn unmapped(xbox: u32) -> u32 {
    miss(format!("tag {xbox:08X}"));
    0
}

/// The PS4 lookups that missed since the last call, which this clears.
pub fn take_misses() -> Vec<String> {
    MISSES
        .lock()
        .map(|mut misses| std::mem::take(&mut *misses).into_iter().collect())
        .unwrap_or_default()
}

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
        // The source statuses whose Duration keys Shadowkeep ports publish (Run for Your Life,
        // In Your Element, Buddy System): the PS4 status hosts with the same key, policy words
        // and HUD identity, found by scanning all 499 PS4 status hosts for each key.
        0x8127_B4EF => return 0x8127_B624,
        0x816E_6703 => return 0x816E_6A06,
        0x80AA_9EAC => return 0x80AA_A3A5,
        // The playable human's animation lookup and first-person pose, whose bank, library and
        // layers the human holding motions read: `808020BB` and `80802790` owners naming the
        // same bank `80A7E499` on PS4.
        0x80A7_E159 => return 0x80A7_E2C0,
        0x80A7_E157 => return 0x80A7_E2BE,
        // The playable human's control set (`8080099B`), the only one in that range on PS4. The
        // holding-motion reader checks it against the native human controls array by array.
        0x80A7_F7F6 => return 0x80A7_F90B,
        // The shared projectile model and body owners, the ones most PS4 weapon projectile
        // graphs name in those roles, as upstream's Xbox One constants name the owners every
        // examined rocket names.
        0x80A7_E38C => return 0x80A7_E4E7,
        0x80A7_E38D => return 0x80A7_E4E8,
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
        // The shared projectile markers, auxiliary, tracking and query owners: those most PS4
        // weapon projectile graphs (70 surveyed) name in each role.
        0x80AA_2D63 => 3,
        0x80AA_8350 => 0x1DB,
        0x80AA_DD5A | 0x80AA_DD5D => 0x1035,
        // The owner outside the graph that rocket inputs read: the only external endpoint (120
        // connection rows) in the PS4 projectile graphs.
        0x80AA_D50E => 0xE59,
        // The damage label dictionary.
        0x80AA_C26E => 0x914,
        0x80AA_C281..=0x80AA_C28C => 0x917,
        0x80AA_C444..=0x80AA_C44C => 0xA07,
        0x80AA_CEA6..=0x80AA_CEA8 => 0xCD7,
        0x80AA_D0F9 => 0xD47,
        0x80AA_D523..=0x80AA_D54B => 0xE59,
        0x80AA_E896 => 0x10FB,
        0x80AA_EDB3..=0x80AA_EDB4 => 0x112C,
        _ => return unmapped(xbox),
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
    use super::{ps4, unmeasured};

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
            _ => unmeasured("status::settings", xbox),
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
            _ => unmeasured("status::hud", xbox),
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
            _ => unmeasured("status::duration", xbox),
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
            _ => unmeasured("status::timer", xbox),
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
        _ => unmeasured("bank", xbox),
    }
}

/// A control set definition (`8080099B`, the source rig's control groups, operations,
/// parameters, slot lookups and named controls) field, given by its Xbox One offset. Every array
/// from the control groups at `+90` on sits 16 bytes earlier on PS4, measured on all 911 PS4
/// control sets (`tools/python/d1/d1classlayout.py 8080099B`).
pub(crate) fn controls(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0x90..=0x110 => xbox - 0x10,
        _ => unmeasured("controls", xbox),
    }
}

/// An animation lookup definition (`808020BB`) field, given by its Xbox One offset. Its tag
/// fields from the clip bank at `+A8` to the human tables at `+BC` sit 24 bytes earlier on PS4.
/// Measured on all 6944 PS4 lookups (`d1classlayout.py 808020BB --tags`): `+90` names
/// `8080408D` bank rows and `+9C` `80802153`/`808023C9` state names and nodes, and the human
/// lookup's `+A0`/`+A4` hold the tables only it has.
pub(crate) fn lookup(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0xA8..=0xBC if xbox % 4 == 0 => xbox - 0x18,
        _ => unmeasured("lookup", xbox),
    }
}

/// A runtime marker set definition (`808008FB`) field, given by its Xbox One offset: the marker
/// rows at `+D8` sit at `+C8` on PS4, measured on all 2090 PS4 marker sets. The rows keep the
/// 64-byte native layout (1319 multi-row sets hold a unit position `w` at that stride).
pub(crate) fn markers(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0xD8 => 0xC8,
        _ => unmeasured("markers", xbox),
    }
}

/// A weapon Barrel definition (`8080221A`, the typed object a weapon owner's Barrel export
/// names) field, given by its Xbox One offset: the two launch modes' projectile graphs at `+9B8`
/// and `+9C8` sit at `+948` and `+958` on PS4, measured on all 98 PS4 Barrels outside the
/// development fallback package (56 name one graph in both modes, 42 none).
pub(crate) fn barrel(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0x9B8 | 0x9C8 => xbox - 0x70,
        _ => unmeasured("barrel", xbox),
    }
}

/// Fields of retained projectile owners that may differ from the shared owner, given by their
/// Xbox One offsets. Each was measured against the PS4 owners upstream's examples name.
pub(crate) mod retained {
    use super::{ps4, unmeasured};

    /// A projectile model owner (`80801A9C`): four floats at `+C0` and the model at `+1C4` sit at
    /// `+A0` and `+15C` on PS4, where Thorn's `80A6A42B` differs from shared `80A7E4E7`.
    pub(crate) fn model(xbox: usize) -> usize {
        match xbox {
            _ if !ps4() => xbox,
            0xC0..0xD0 => xbox - 0x20,
            0x1C4..0x1C8 => xbox - 0x68,
            _ => unmeasured("retained::model", xbox),
        }
    }

    /// An attachment spawner (`80802A9A`): the flags, scale and extents block at `+120` and the
    /// published name with its count at `+410` sit at `+F0` and `+3D8` on PS4 (Zhalo Supercell's
    /// `80AAED68`: flags `0x101`, scale 1.0, extents 175.0 and name `5D64368A`, count 1).
    pub(crate) fn spawner(xbox: usize) -> usize {
        match xbox {
            _ if !ps4() => xbox,
            0x120 => 0xF0,
            0x410 => 0x3D8,
            _ => unmeasured("retained::spawner", xbox),
        }
    }

    /// A projectile provider (`808030BA`): the published name at `+A8` and the block of 0.3, zero,
    /// value and count at `+B0` sit `0x18` earlier on PS4 (`80AAFC04`: name `CD7422C6`, 60.0).
    pub(crate) fn provider(xbox: usize) -> usize {
        match xbox {
            _ if !ps4() => xbox,
            0xA8 | 0xB0 => xbox - 0x18,
            _ => unmeasured("retained::provider", xbox),
        }
    }

    /// A projectile target query (`80802947`): the scale at `+450` sits at `+418` on PS4, where
    /// `80AAF3A4` holds 1.3 and shared `80AAED92` 1.0, the owners' only difference.
    pub(crate) fn query(xbox: usize) -> usize {
        match xbox {
            _ if !ps4() => xbox,
            0x450 => 0x418,
            _ => unmeasured("retained::query", xbox),
        }
    }
}

/// A damage impulse definition (`c_damage_impulse_definition`, PS4 schema tag `80AACBA2`)
/// field, given by its Xbox One offset. PS4 starts the description at `+78`, eight bytes before
/// Xbox One's `+80`, as the class reflection lists it, and keeps the same order after it (the
/// damage labels at `+C0`, Xbox One `+C8`); the fields before it keep their Xbox One offsets. The
/// PS4 definition is `0x110` bytes, Xbox One's `0x120`: its tail padding differs as well, so the
/// size is not a field.
pub(crate) fn impulse(xbox: usize) -> usize {
    match xbox {
        _ if !ps4() => xbox,
        0..0x78 => xbox,
        _ => xbox - 8,
    }
}

/// A projectile movement definition (`808020AC`) field, given by its Xbox One offset. Every
/// field the readers take up to the child parameter outputs at `+1A0` sits where Xbox One keeps
/// it (limits `+8C`/`+8D`, the profile at `+128`, computed output `+138`, outputs of `808008F1`
/// rows at `+1A0`). The component slot declaration after them packs, so every later field sits 8
/// bytes earlier: the resource fields at `+1E0`, `+1F0`, `+200` and `+210` (PS4 `+1E0` is zero in
/// every owner), the response groups (`8080239D`) at `+218` and the scalar expressions
/// (`808005A0`) at `+228`. Measured on the 651 PS4 movement owners outside the development
/// fallback package (`80B00xxx`, whose 50 owners keep an older layout). Group, node and scalar
/// rows keep the Xbox One layout.
pub(crate) fn movement(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0x8C | 0x8D | 0x128 | 0x138 | 0x1A0 => xbox,
        0x1E0 | 0x1F0 | 0x200 | 0x210 | 0x220 | 0x230 => xbox - 8,
        _ => unmeasured("movement", xbox),
    }
}

/// A weapon content resource definition (`8080248D`) field, given by its Xbox One offset: the
/// default content row at `+B0` and the keyed variant rows at `+1F0` sit `0x28` earlier on PS4,
/// measured on all 456 PS4 content resources (variants of class `80802312` at `+1C8` in every
/// owner that has any; the default row's slot entities, parent and graph at `+C0`..`+120`). Both
/// row forms keep the 320-byte Xbox One row layout (key `+18`, slots `+38`, parent `+78`, graph
/// `+98`).
pub(crate) fn content(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0xB0 | 0x1F0 => xbox - 0x28,
        _ => unmeasured("content", xbox),
    }
}

/// A connection input (an `80800862` input declaration with its `8080052A` class, as light pose
/// selections, optic aim providers and inherited aim inputs hold) field, given by its Xbox One
/// offset from the input's start. Xbox One keeps the declaration's zero and class as u64s at
/// `+18`/`+20`; PS4 packs them into u32s at `+18`/`+1C`, so every later field sits 8 bytes
/// earlier: the default rows at `+48` sit at `+40`. All of the ~19,000 such declarations in the
/// 117,689 PS4 owners use the packed form.
pub(crate) fn input(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0..0x18 => xbox,
        0x28..=0x50 => xbox - 8,
        _ => unmeasured("input", xbox),
    }
}

/// Whether a connection input's declaration class is `8080052A` with its zero word beside it:
/// Xbox One's u64 class at `+20` (zero upper half at `+24`), PS4's zero at `+18` and class at
/// `+1C`.
pub(crate) fn input_class(
    p: &crate::tiger::payload::Payload,
    start: usize,
) -> anyhow::Result<bool> {
    Ok(if ps4() {
        p.u32(start + 0x18)? == 0 && p.u32(start + 0x1C)? == 0x8080_052A
    } else {
        p.u32(start + 0x20)? == 0x8080_052A && p.u32(start + 0x24)? == 0
    })
}

/// A query definition (`80803136`) field, given by its Xbox One offset. Its provider declarations
/// (pointer, tag, name) pack on PS4: the two at `+60`/`+78` take 16 bytes each, so the value pair
/// at `+90`, the filter groups at `+98`, the outputs at `+A8`, the embedded provider at `+C0`, the
/// target search at `+D8` and the embedded declaration at `+F0` sit 16 bytes earlier; the named
/// declarations at `+108`, `+128` and `+148` take 24 bytes each (name at `+10`, Xbox One `+18`),
/// so they sit at `+F0`, `+108` and `+120`, and the trailing outputs at `+168` at `+138`.
/// Measured on all 9 PS4 query owners (`d1classlayout.py 80803136`): every declaration points at
/// the definition (the embedded one at `+B0`), all 9 hold the search's finite values, flag and
/// zero at `+C8`, and eight hold 24 zero trailing bytes where `80A88E30` holds two pointers to
/// `80801108` output objects. The group, filter, term and output rows keep their Xbox One layout
/// (`80A79CC0`).
pub(crate) fn query(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0x60 => 0x60,
        0x78 => 0x70,
        0x90..=0xA8 | 0xC0 | 0xD8 | 0xF0 => xbox - 0x10,
        0x108 => 0xF0,
        0x128 => 0x108,
        0x148 => 0x120,
        0x168 => 0x138,
        _ => unmeasured("query", xbox),
    }
}

/// Whether a channel getter definition (`80800668`) names its state class `first` and instance
/// class `second` with the zero between them. Xbox One keeps both classes and the zero as u64s at
/// `+10`, `+18` and `+20` of a 40-byte row; PS4 packs them into a 32-byte row: `first` as a u32 at
/// `+10`, the zero as a u64 at `+14` and `second` as a u32 at `+1C` (`getter_stride`). Measured
/// on the PS4 receiver banks, whose runtime states keep the Xbox One layout (15,908 states).
pub(crate) fn getter_classes(
    p: &crate::tiger::payload::Payload,
    at: usize,
    first: u32,
    second: u32,
) -> anyhow::Result<bool> {
    Ok(if ps4() {
        p.u32(at + 0x10)? == first && p.u64(at + 0x14)? == 0 && p.u32(at + 0x1C)? == second
    } else {
        p.u64(at + 0x10)? == u64::from(first)
            && p.u64(at + 0x18)? == 0
            && p.u64(at + 0x20)? == u64::from(second)
    })
}

/// A channel getter definition row's stride: 40 bytes on Xbox One, 32 on PS4.
pub(crate) fn getter_stride() -> usize {
    field(40, 32)
}

/// An optic definition (`808027FC`) value-provider field, given by its Xbox One offset: the
/// provider at `+2B0` (a pointer to the definition), its metadata tag at `+2B8` and its name at
/// `+2C8` sit at `+280`, `+288` and `+290` on PS4, where the provider's declaration packs. Measured
/// on all 352 PS4 optics: each points back to its definition at `+280`, names metadata `80A3D0F1`
/// at `+288` and the sight channel `4F437B90` at `+290`.
pub(crate) fn optic(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0x2B0 => 0x280,
        0x2B8 => 0x288,
        0x2C8 => 0x290,
        _ => unmeasured("optic", xbox),
    }
}

/// A first-person pose definition (`80802790`) field, given by its Xbox One offset: the clip bank
/// at `+108`, the parameter library at `+10C` and the layer dispatch at `+118` sit 40 bytes
/// earlier on PS4. Measured on the human pose `80A7E2BE`, whose bank equals its lookup's, whose
/// library holds `808020B4` rows and whose layers cover the bank's 1142 clips.
pub(crate) fn pose(xbox: usize) -> usize {
    if !ps4() {
        return xbox;
    }
    match xbox {
        0x108 | 0x10C | 0x118 => xbox - 0x28,
        _ => unmeasured("pose", xbox),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiger::payload::Payload;

    /// Every offset and tag the readers can pass, in the ranges any source structure spans.
    const OFFSETS: std::ops::Range<usize> = 0..0x2000;

    /// A payload whose every byte is its own offset, so each read names where it came from.
    fn numbered() -> Payload {
        Payload((0..0x400).map(|at| at as u8).collect())
    }

    /// The Xbox One build keeps upstream's values. Every helper returns its Xbox One argument
    /// unchanged and every payload helper reads the Xbox One field, so no PS4 table can reach an
    /// Xbox One import. The PS4 values are spot checks of measured layouts. The flag is global,
    /// so one test sets it and leaves the Xbox One default; no other unit test opens a source.
    #[test]
    fn xbox_one_values_pass_through_and_ps4_tables_hold() -> anyhow::Result<()> {
        set(8);
        assert!(!ps4());
        for x in OFFSETS {
            assert_eq!(field(x, x + 1), x);
            assert_eq!(sequence(x), x);
            assert_eq!(bank(x), x);
            assert_eq!(status::settings(x), x);
            assert_eq!(status::hud(x), x);
            assert_eq!(status::duration(x), x);
            assert_eq!(status::timer(x), x);
            assert_eq!(controls(x), x);
            assert_eq!(lookup(x), x);
            assert_eq!(pose(x), x);
            assert_eq!(markers(x), x);
            assert_eq!(content(x), x);
            assert_eq!(movement(x), x);
            assert_eq!(impulse(x), x);
            assert_eq!(barrel(x), x);
            assert_eq!(retained::model(x), x);
            assert_eq!(retained::spawner(x), x);
            assert_eq!(retained::provider(x), x);
            assert_eq!(retained::query(x), x);
            assert_eq!(input(x), x);
            assert_eq!(query(x), x);
        }
        for xbox in (0x80A0_0000..0x80B2_0000).chain([0x8080_0000, 0x8080_2698, 0x80C7_09E1]) {
            assert_eq!(tag(xbox), xbox);
        }
        assert_eq!(input_row(), (48, 0x28));
        assert_eq!(receiver_rows(), (0x90, 0x50, 0x30, 0x40));
        let p = numbered();
        let word = |at: usize| p.u64(at);
        assert_eq!(declared(&p, 0x40)?, (word(0x48)?, word(0x50)?));
        assert_eq!(event_kind(&p, 0x40)?, p.u32(0x5C)?);
        assert_eq!(status::receiver(&p, 0x40)?, (word(0x110)?, word(0x118)?));
        // Xbox One's input class check reads the u64 class at +20 (the numbered payload holds
        // no class, so the check reads the Xbox One words and fails).
        assert!(!input_class(&p, 0x40)?);

        set(7);
        let measured = [
            (sequence(0x160), 0x110),
            (sequence(0x2A8), 0x240),
            (bank(0xD8), 0xB8),
            (status::settings(0xF0), 0xD0),
            (status::hud(0x160), 0x140),
            (status::duration(0x138), 0xF0),
            (status::timer(0x80), 0x60),
            (input_row().1, 0x20),
            (controls(0x90), 0x80),
            (lookup(0xB4), 0x9C),
            (pose(0x118), 0xF0),
            (markers(0xD8), 0xC8),
            (content(0xB0), 0x88),
            (content(0x1F0), 0x1C8),
            (movement(0x220), 0x218),
            (movement(0x230), 0x228),
            (impulse(0x28), 0x28),
            (impulse(0xC8), 0xC0),
            (impulse(0x108), 0x100),
            (barrel(0x9B8), 0x948),
            (retained::model(0x1C4), 0x15C),
            (retained::spawner(0x410), 0x3D8),
            (retained::provider(0xB0), 0x98),
            (retained::query(0x450), 0x418),
            (input(0x48), 0x40),
            (query(0xA8), 0x98),
        ];
        set(8);
        for (ps4, expected) in measured {
            assert_eq!(ps4, expected);
        }
        set(7);
        let tags = [
            tag(0x80AA_CEA6),
            tag(0x80AA_C28B),
            tag(0x80A7_E1DF),
            tag(0x80AA_0001),
        ];
        let unmeasured = sequence(0x70);
        set(8);
        assert_eq!(tags, [0x80AA_DB7D, 0x80AA_CBA2, 0x80A7_E344, 0]);
        assert_eq!(unmeasured, UNMEASURED);
        let misses = take_misses();
        assert!(misses.contains(&"sequence 0x70".to_owned()));
        assert!(misses.contains(&"tag 80AA0001".to_owned()));
        assert!(take_misses().is_empty());
        assert!(!ps4());
        Ok(())
    }
}
