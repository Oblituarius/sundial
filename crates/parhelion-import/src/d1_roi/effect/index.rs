//! Every stock Shadowkeep effect graph by the shape the translator matches source graphs on.
//!
//! The translator picks native templates by owner family ([`super::choice`]), and the dedicated
//! attachment routes pick whole stock graphs by family, row count and connections. This index
//! reads every stock graph (`80809C0F`) with the translator's own family reading, so a search
//! over it finds the stock counterparts of a source shape the way the translator would see them.
//! `tools/python/sk/skgraphs.py` queries the written index.
use super::*;

/// Every stock graph: its owners with their family, classes and modifier row count, its
/// connections between owner families, and whether the translator reads it as a template. A
/// graph that does not read is listed with its error.
pub fn stock_graph_index(native: &mut Reader) -> Result<Vec<Value>> {
    let mut tags = native.classes(0x80809C0F);
    tags.sort_unstable();
    let mut index = Vec::with_capacity(tags.len());
    for (count, tag) in tags.into_iter().enumerate() {
        index.push(match graph(native, tag) {
            Ok(entry) => entry,
            Err(error) => json!({"tag": format!("{tag:08X}"), "error": format!("{error:#}")}),
        });
        // Stock graphs share few owners; keep the reader's cache from holding the whole build.
        if count % 2000 == 1999 {
            native.clear_cached_tags();
        }
    }
    Ok(index)
}

fn graph(native: &mut Reader, tag: u32) -> Result<Value> {
    let graph = (*native.tag(tag, Some(0x80809C0F))?).clone();
    // A graph whose connections the reader refuses (a null controller endpoint) still lists its
    // owners, which a whole-graph reuse needs; its connections are left out with the reason.
    let (links, links_error) = match links::Graph::read(&graph, false) {
        Ok(links) => (links, None),
        Err(error) => {
            let components = graph
                .array(16, 12, Some(0x80809C04))?
                .into_iter()
                .map(|row| graph.u32(row))
                .collect::<Result<Vec<_>>>()?;
            let links = links::Graph {
                components,
                connections: Vec::new(),
                named_connections: Vec::new(),
            };
            (links, Some(format!("{error:#}")))
        }
    };
    let mut families = Vec::new();
    let mut owners = Vec::new();
    for &owner in &links.components {
        let (family, entry) = match native.tag(owner, Some(0x80809C36)) {
            Ok(payload) => owner_entry(native, owner, &payload),
            Err(error) => (
                None,
                json!({"tag": format!("{owner:08X}"), "error": format!("{error:#}")}),
            ),
        };
        families.push(family.map(Family::name));
        owners.push(entry);
    }
    let end = |endpoint: &links::Endpoint| -> Value {
        match usize::try_from(endpoint.selector) {
            Ok(ordinal) if ordinal < families.len() => json!({
                "ordinal": ordinal,
                "family": families[ordinal],
                "class": endpoint.object.map(|o| format!("{:08X}", o.class)),
                "offset": endpoint.object.map(|o| o.offset),
            }),
            _ if endpoint.selector == 0xFFFF => json!("disconnected"),
            _ => json!(format!("external {:X}", endpoint.selector)),
        }
    };
    let connections = |rows: &[links::Connection]| -> Vec<Value> {
        rows.iter()
            .map(|row| {
                json!({
                    "consumer": end(&row.consumer),
                    "provider": end(&row.provider),
                    "channel": row.channel,
                    "flags": row.flags,
                })
            })
            .collect()
    };
    Ok(json!({
        "tag": format!("{tag:08X}"),
        "families": families,
        "owners": owners,
        "connections": connections(&links.connections),
        "named_connections": connections(&links.named_connections),
        "links_error": links_error,
        "template": match links_error {
            Some(_) => json!("connections unread"),
            None => match Template::read(native, tag) {
                Ok(_) => json!(true),
                Err(error) => json!(format!("{error:#}")),
            },
        },
    }))
}

fn owner_entry(native: &mut Reader, tag: u32, owner: &Payload) -> (Option<Family>, Value) {
    let classes = pair(owner).and_then(|(instance, definition)| {
        Ok((
            owner.u32(instance - 4)?,
            owner.u32(definition - 4)?,
            definition,
        ))
    });
    let Ok((instance, definition_class, definition)) = classes else {
        return (
            None,
            json!({"tag": format!("{tag:08X}"), "error": "owner roots"}),
        );
    };
    let family = native_family(native, owner).ok();
    // A modifier bank's live rows, the count the dedicated routes select their carrier by.
    let rows = (definition_class == 0x80803AFE)
        .then(|| owner.array(definition + 88, 88, Some(0x80803B06)).ok())
        .flatten()
        .map(|rows| rows.len());
    (
        family,
        json!({
            "tag": format!("{tag:08X}"),
            "family": family.map(Family::name),
            "instance": format!("{instance:08X}"),
            "definition": format!("{definition_class:08X}"),
            "size": owner.0.len(),
            "rows": rows,
        }),
    )
}
