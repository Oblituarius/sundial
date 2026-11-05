//! Match stored Xbox stream declarations to actual native declarations.
use super::*;
use crate::tiger::reader::Reader;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Element {
    semantic: u8,
    index: u8,
    format: u8,
}

type Streams = [Vec<Element>; 4];

pub(super) struct Layouts {
    source: BTreeMap<u16, Streams>,
    native: BTreeMap<u16, Streams>,
}

fn declarations(
    elements: &Payload,
    mapping: &Payload,
    source: bool,
) -> Result<BTreeMap<u16, Streams>> {
    let elements = elements
        .array(8, 16, None)?
        .into_iter()
        .map(|row| {
            elements
                .array(row, 3, None)?
                .into_iter()
                .map(|at| {
                    let format = elements.u8(at + 2)?;
                    Ok(Element {
                        semantic: elements.u8(at)?,
                        index: elements.u8(at + 1)?,
                        format: if source {
                            match format {
                                9 => 10,
                                10 => 11,
                                11 => 12,
                                other => other,
                            }
                        } else {
                            format
                        },
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let mut result = BTreeMap::new();
    for row in mapping.array(8, 28, None)? {
        // The armor path does not consume hardware-instanced declarations.
        if mapping.bytes::<4>(row + 24)? != [0; 4] {
            continue;
        }
        let mut streams: Streams = std::array::from_fn(|_| Vec::new());
        for (index, stream) in streams.iter_mut().enumerate() {
            let selected = mapping.u32(row + 8 + index * 4)?;
            if selected != u32::MAX {
                *stream = elements
                    .get(selected as usize)
                    .context("Vertex element set is absent")?
                    .clone();
            }
        }
        ensure!(
            result.insert(mapping.u16(row)?, streams).is_none(),
            "Repeated stream layout identity"
        );
    }
    Ok(result)
}

impl Source {
    pub(super) fn named(&self, name: &str) -> Result<u32> {
        let tags = self
            .packages
            .values()
            .flat_map(|package| package.reader.named_tags())
            .filter(|tag| tag.name == name)
            .map(|tag| tag.hash.0)
            .collect::<std::collections::BTreeSet<_>>();
        ensure!(tags.len() == 1, "Missing or ambiguous source name {name}");
        Ok(*tags.first().unwrap())
    }
}

impl Layouts {
    pub fn read(source: &Source, native: &mut Reader) -> Result<Self> {
        let bootstrap = source.read(source.named("client_bootstrap_patchable")?)?;
        let globals = source.read(bootstrap.u32(0x3C)?)?;
        let root = source.read(globals.u32(8)?)?;
        let elements = source.read(root.u32(0x10)?)?;
        let mappings = source.read(root.u32(0x28)?)?;
        let source = declarations(&elements, &mappings, true)?;
        let named = native
            .manager
            .lookup
            .named_tags
            .iter()
            .find(|tag| tag.name == "client_bootstrap_patchable")
            .context("Native bootstrap name is absent")?
            .hash
            .0;
        let bootstrap = native.tag(named, None)?;
        let globals = native.tag(bootstrap.u32(0x4C)?, None)?;
        let root = native.tag(globals.u32(8)?, Some(0x808072A6))?;
        let elements = native.tag(root.u32(12)?, None)?;
        let mappings = native.tag(root.u32(40)?, None)?;
        Ok(Self {
            source,
            native: declarations(&elements, &mappings, false)?,
        })
    }

    pub fn matching(&self, source: u8, strides: [usize; 4]) -> Result<u16> {
        let layout = self
            .source
            .get(&u16::from(source))
            .context("Missing source stream declaration")?;
        for (stream, stride) in layout.iter().zip(strides) {
            let mut size = 0;
            for element in stream {
                size += match element.format {
                    4 => 16,
                    5 | 6 | 10 | 12 => 4,
                    11 => 8,
                    other => anyhow::bail!("Unsupported source vertex element format {other}"),
                };
            }
            ensure!(
                size == stride,
                "Source declaration and stored vertex stride differ"
            );
        }
        self.native
            .iter()
            .find(|(_, candidate)| *candidate == layout)
            .map(|(index, _)| *index)
            .context("Source stream declaration has no byte-compatible native layout")
    }

    /// The stream, byte offset and native format of a native layout's element.
    pub fn element(
        &self,
        layout: u16,
        semantic: u8,
        index: u8,
    ) -> Result<Option<(usize, usize, u8)>> {
        let streams = self
            .native
            .get(&layout)
            .context("Missing native stream declaration")?;
        for (stream, elements) in streams.iter().enumerate() {
            let mut offset = 0;
            for element in elements {
                if element.semantic == semantic && element.index == index {
                    return Ok(Some((stream, offset, element.format)));
                }
                offset += match element.format {
                    1 | 5 | 6 | 7 | 10 | 12 | 14 | 15 | 16 | 17 | 18 | 21 | 31 | 32 => 4,
                    2 | 8 | 9 | 11 | 13 | 19 | 22 | 33 => 8,
                    3 => 12,
                    4 | 20 | 23 => 16,
                    24 => 2,
                    25 => 1,
                    other => anyhow::bail!("Unsupported native vertex element format {other}"),
                };
            }
        }
        Ok(None)
    }

    /// A native layout's elements, (semantic, index) in stream order: the order its vertex
    /// buffer descriptors, and so a PS4 fetch shader's inputs, follow.
    pub fn ordered(&self, layout: u16) -> Result<Vec<(u8, u8)>> {
        Ok(self
            .native
            .get(&layout)
            .context("Missing native stream declaration")?
            .iter()
            .flatten()
            .map(|e| (e.semantic, e.index))
            .collect())
    }

    fn elements(&self, layout: u16) -> Option<Vec<(u8, u8, u8)>> {
        self.native.get(&layout).map(|streams| {
            let mut all = streams
                .iter()
                .flatten()
                .map(|e| (e.semantic, e.index, e.format))
                .collect::<Vec<_>>();
            all.sort_unstable();
            all
        })
    }

    /// Whether two native layouts declare the same elements, whatever their stream split.
    /// A vertex program binds its inputs by semantic, so either layout feeds it.
    pub fn same_elements(&self, a: u16, b: u16) -> bool {
        self.elements(a)
            .is_some_and(|a| Some(a) == self.elements(b))
    }

    /// Whether `layout` declares `stock`'s elements plus only vertex colour (semantic 8), so
    /// a vertex program written for `stock`, which reads no colour, binds every input from it.
    pub fn feeds(&self, layout: u16, stock: u16) -> bool {
        let (Some(mut layout), Some(stock)) = (self.elements(layout), self.elements(stock)) else {
            return false;
        };
        layout.retain(|&(semantic, ..)| semantic != 8);
        layout == stock
    }
}
