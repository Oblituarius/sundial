//! Source weapon ornaments as private appearance choices in the native donor's ornament socket.
//!
//! The socket offers only the reset and the converted ornaments: the donor's own ornaments are
//! removed. Each ornament's art converts the way the weapon's own parts do, on the same native runtime
//! and bone binding, into a graph attached to the weapon's. The graph names the private plug
//! copied from one native ornament of the socket, which keeps that plug's category and the
//! socket's own default-appearance reset. An ornament carries model art, dyes and an icon only:
//! animation, audio and gameplay stay with the weapon.
use super::super::recipe::hash;
use super::*;

/// The weapon being prepared, which its ornaments convert against.
pub(super) struct Host<'a> {
    pub weapon: &'a Weapon,
    pub source: &'a Value,
    pub native: &'a Path,
    pub donor: u32,
    pub template: u16,
    pub private_item: u32,
    pub namespace: &'a str,
}

/// A donor socket that holds ornaments.
struct Socket {
    index: usize,
    socket_type: u64,
    reset: u32,
    plugs: Vec<u32>,
}

/// The converted ornaments: their socket column, private choices and attached graphs.
pub(super) struct Converted {
    pub socket: usize,
    pub column: Value,
    pub variants: Vec<Value>,
    pub attachments: Vec<GraphReference>,
    pub report: Value,
}

fn word(value: &Value, what: &str) -> Result<u32> {
    u32::try_from(value.as_u64().with_context(|| format!("{what} missing"))?).map_err(Into::into)
}

/// The donor socket that offers a default reset and at least `count` (and at least one)
/// ornaments with art.
fn ornament_socket(sockets: &Value, count: usize) -> Result<Option<Socket>> {
    let count = count.max(1);
    for socket in sockets["sockets"]
        .as_array()
        .context("Native socket report")?
    {
        let choices = socket["choices"]
            .as_array()
            .context("Native socket choices")?;
        let appearance = |c: &&Value| {
            c["item_kind"] == 13 && c["art_indices"].as_array().is_some_and(|a| !a.is_empty())
        };
        let Some(reset) = choices.iter().find(|c| {
            c["default"] == true
                && c["item_kind"] == 13
                && c["art_indices"].as_array().is_some_and(Vec::is_empty)
        }) else {
            continue;
        };
        let plugs = choices
            .iter()
            .filter(|c| c["default"] != true)
            .filter(appearance)
            .map(|c| word(&c["hash"], "Native ornament"))
            .collect::<Result<Vec<_>>>()?;
        if plugs.len() < count {
            continue;
        }
        let index = usize::try_from(socket["index"].as_u64().context("Native socket index")?)?;
        let socket_type = socket["socket_type"]
            .as_u64()
            .context("Native socket type")?;
        return Ok(Some(Socket {
            index,
            socket_type,
            reset: word(&reset["hash"], "Native reset")?,
            plugs,
        }));
    }
    Ok(None)
}

fn donor_sockets(native: &Path, donor: u32, output: &Path) -> Result<Value> {
    let mut reader = Reader::discovery(native, &output.join("ornament-sockets"), false)?;
    let sockets = crate::tiger::shadowkeep::sockets(&mut reader, donor)?;
    reader.finish()?;
    Ok(sockets)
}

fn column(choices: Vec<Value>, socket_type: u64) -> Value {
    json!({"choices":choices,"socket_type":socket_type,"choice_conditions":[],
        "choice_weight_bits":[],"randomized_selection_program":[]})
}

/// The donor's ornament socket without the donor's own ornaments, so the imported weapon never
/// offers them: reduced to its default-appearance reset with `keep_reset`, otherwise disabled
/// the way a native unused socket row is. `None` when the donor has no ornament socket or the
/// socket already holds translated talents.
fn without_source(
    native: &Path,
    donor: u32,
    columns: &[Value],
    output: &Path,
    keep_reset: bool,
) -> Result<Option<(usize, Value)>> {
    let sockets = donor_sockets(native, donor, output)?;
    let Some(Socket {
        index: socket,
        socket_type,
        reset,
        ..
    }) = ornament_socket(&sockets, 1)?
    else {
        return Ok(None);
    };
    if !columns.get(socket).is_none_or(Value::is_null) {
        return Ok(None);
    }
    Ok(Some((
        socket,
        if keep_reset {
            column(vec![json!(format!("0x{reset:08X}"))], socket_type)
        } else {
            json!({"choices":[],"socket_type":u16::MAX})
        },
    )))
}

impl Source {
    /// The donor's ornament socket for the imported weapon: its converted source ornaments
    /// after the reset. The donor's own ornaments never remain. A weapon without source
    /// ornaments loses the socket, and a failed conversion keeps only the reset and says why
    /// in `limitations`. `None` when the donor has no ornament socket to change.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn weapon_ornament_socket(
        &self,
        host: &Host,
        columns: &[Value],
        output: &Path,
        options: &NativeOptions,
        limitations: &mut Vec<String>,
        progress: &mut dyn FnMut(String),
    ) -> Result<Option<Converted>> {
        let keep_reset =
            match self.convert_weapon_ornaments(host, columns, output, options, progress) {
                Ok(Some(converted)) => return Ok(Some(converted)),
                Ok(None) => false,
                Err(error) => {
                    progress(format!("Ornaments not converted: {error:#}"));
                    limitations.push(format!("Source ornaments were not converted: {error:#}"));
                    true
                }
            };
        Ok(
            without_source(host.native, host.donor, columns, output, keep_reset)?.map(
                |(socket, column)| Converted {
                    socket,
                    column,
                    variants: Vec::new(),
                    attachments: Vec::new(),
                    report: json!({"socket":socket,"choices":[],"donor_ornaments_removed":true,
                        "socket_disabled":!keep_reset}),
                },
            ),
        )
    }

    /// Convert every source ornament into a private choice of the donor's ornament socket.
    /// `Ok(None)` when the weapon has no ornaments.
    fn convert_weapon_ornaments(
        &self,
        host: &Host,
        columns: &[Value],
        output: &Path,
        options: &NativeOptions,
        progress: &mut dyn FnMut(String),
    ) -> Result<Option<Converted>> {
        let Host {
            weapon,
            source,
            native,
            donor,
            template,
            private_item,
            namespace,
        } = *host;
        let mut assets = BTreeMap::new();
        let ornaments = self.weapon_ornaments(weapon, &mut assets)?;
        if ornaments.is_empty() {
            return Ok(None);
        }
        progress(format!(
            "Converting {} ornaments for {}",
            ornaments.len(),
            weapon.name
        ));
        let sockets = donor_sockets(native, donor, output)?;
        let Socket {
            index: socket,
            socket_type,
            reset,
            plugs,
        } = ornament_socket(&sockets, ornaments.len())?.with_context(|| {
            format!(
                "Native donor {donor:08X} has no ornament socket with {} appearance choices",
                ornaments.len()
            )
        })?;
        ensure!(
            columns.get(socket).is_none_or(Value::is_null),
            "Native ornament socket {socket} already holds translated talents"
        );
        let weapon_dyes = source["dyes"].as_array().context("Source weapon dyes")?;
        let mut choices = vec![json!(format!("0x{reset:08X}"))];
        let mut variants = Vec::new();
        let mut attachments = Vec::new();
        let mut report = Vec::new();
        for (index, ornament) in ornaments.iter().enumerate() {
            let choice = index + 1;
            let plug = plugs[index];
            let info = &ornament["item"];
            let definition = word(&info["definition"], "Ornament definition")?;
            let strings = word(&info["strings"], "Ornament strings")?;
            let target = hash(&format!(
                "parhelion/{namespace}/socket/{socket}/choice/{choice}/private-plug"
            ));
            let directory = output.join(format!("ornament-{index}"));
            let mut assembly = Assembly::new(
                self,
                native,
                &directory,
                options,
                Some((&source["runtime"], donor)),
            )?;
            let (label, container) = assembly.ornament_display(weapon.definition, ornament)?;
            let mut name = info["name"].as_str().context("Ornament name")?.to_owned();
            if !label.is_empty()
                && super::super::localization::Labels::new(self)?
                    .text(self, &self.read(strings)?, 0x78)?
                    .is_empty()
            {
                name = label;
            }
            // An ornament may override only some of the weapon's dye slots. Keep the rest and
            // the layer precedence.
            let mut dyes = weapon_dyes.clone();
            dyes.extend(self.dyes(definition, &mut assets)?);
            dyes.sort_by_key(|dye| match dye["layer"].as_str() {
                Some("default") => 0,
                Some("custom") => 1,
                _ => 2,
            });
            let mut parts = Vec::new();
            for (i, part) in ornament["art"]["parts"]
                .as_array()
                .context("Ornament parts")?
                .iter()
                .enumerate()
            {
                progress(format!("Converting ornament {name} part {}", i + 1));
                let symbol = assembly.body(part, &dyes, &format!("part-{i}"))?;
                parts.push(json!({"source_assignment":part["assignment"],
                    "key":key(&format!("parhelion/imported-gear/{target:08X}/art-{i}")),"parent":symbol}));
            }
            ensure!(!parts.is_empty(), "Ornament {name} has no converted art");
            let mut rows = ornament["art"]["rows"]
                .as_array()
                .context("Ornament art rows")?
                .clone();
            for row in &mut rows {
                row["template_index"] = json!(template);
            }
            let icon = match assembly.item_icon(strings)? {
                Some(icon) => Some(icon),
                None => assembly.container_icon(container)?,
            };
            let mut limitations = vec![
                "The ornament changes model art, dyes and icon. Animation, audio and gameplay stay with the weapon.".to_owned(),
                "Rendering and repeated ornament selection require in-game acceptance.".to_owned(),
            ];
            limitations.extend(assembly.approximations.iter().cloned());
            let graph = json!({"kind":"weapon","source":"d1_roi","source_item":info["hash"],"source_name":name,
                "source_builds":self.report.builds,"source_stamp":self.report.stamp,
                "item_hash":target,"native_item":donor,"converter_revision":crate::graph::CONVERTER_REVISION,
                "nodes":assembly.nodes,"parent":parts[0]["parent"],"dyes":[],"dye_rows":[[],[],[]],
                "gear_art":{"selector_format":"shadowkeep","rows":rows,"parts":parts},
                "ornament_icon_png":icon,"rendering":assembly.evidence,
                "ornament":{"target_item":private_item,"target_socket":socket,"choice_index":choice,
                    "native_plug":plug,"default_reset":reset,"name":name},
                "installable":true,"gameplay_verified":false,"limitations":limitations});
            crate::io::write_json(&directory.join("asset-graph.json"), &graph)?;
            assembly.native.finish()?;
            attachments.push(GraphReference::new(&directory, private_item)?);
            choices.push(json!(format!("0x{plug:08X}")));
            variants.push(json!({"socket_index":socket,"choice_index":choice,
                "source_plug_hash":format!("0x{plug:08X}"),"name":name,
                "description":"Changes this weapon's appearance. Select the default ornament to restore its original appearance.",
                "replace_effects":true,"sandbox_perks":[]}));
            report.push(json!({"source_item":info["hash"],"name":name,"choice":choice,
                "native_plug":plug,"private_plug":target,"node":ornament["node"],"step":ornament["step"]}));
        }
        self.unchanged()?;
        Ok(Some(Converted {
            socket,
            column: column(choices, socket_type),
            variants,
            attachments,
            report: json!({"socket":socket,"socket_type":socket_type,"default_reset":reset,"choices":report}),
        }))
    }
}
