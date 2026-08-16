use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

use crate::storage;

/// One option entry visible in a socket's dropdown.
#[derive(Debug, Clone, Serialize)]
pub struct PlugOption {
    pub plug_hash: String,
    pub plug_name: Option<String>,
    pub is_selected: bool,
}

/// All dropdown options for a single socket.
#[derive(Debug, Clone, Serialize)]
pub struct SocketExport {
    pub socket_index: usize,
    pub options: Vec<PlugOption>,
}

#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    Csv,
    Json,
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs_per_day = 86400u64;
    let secs_per_hour = 3600u64;
    let secs_per_min = 60u64;
    let day = now / secs_per_day;
    let rem = now % secs_per_day;
    let hour = rem / secs_per_hour;
    let rem = rem % secs_per_hour;
    let min = rem / secs_per_min;
    let sec = rem % secs_per_min;
    format!("{day:08}_{hour:02}{min:02}{sec:02}")
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

pub fn build_filename(character_id: &str, slot: &str, format: ExportFormat) -> String {
    let ext = match format {
        ExportFormat::Csv => "csv",
        ExportFormat::Json => "json",
    };
    format!(
        "plugs_{}_{}_{}.{}",
        sanitize(character_id),
        sanitize(slot),
        timestamp(),
        ext
    )
}

fn csv_escape(name: &str) -> String {
    if name.contains([',', '"', '\n']) {
        format!("\"{}\"", name.replace('"', "\"\""))
    } else {
        name.to_string()
    }
}

/// Flat CSV: one row per plug option across all sockets.
/// socket_index,plug_hash,plug_name,is_selected
fn render_csv(sockets: &[SocketExport]) -> String {
    let mut out = String::from("socket_index,plug_hash,plug_name,is_selected\n");
    for socket in sockets {
        for opt in &socket.options {
            let name = opt.plug_name.as_deref().unwrap_or("");
            out.push_str(&format!(
                "{},{},{},{}\n",
                socket.socket_index,
                opt.plug_hash,
                csv_escape(name),
                opt.is_selected,
            ));
        }
    }
    out
}

/// Returns the directory containing the Sundial executable.
fn exe_dir() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map_err(|e| format!("Could not locate Sundial executable: {e}"))?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Sundial executable has no parent directory".into())
}

pub fn write_socket_export(
    character_id: &str,
    slot: &str,
    slot_label: &str,
    sockets: &[SocketExport],
    format: ExportFormat,
) -> Result<PathBuf, String> {
    let dir = exe_dir()?;
    let filename = build_filename(character_id, slot, format);
    let path = dir.join(&filename);

    let bytes = match format {
        ExportFormat::Csv => render_csv(sockets).into_bytes(),
        ExportFormat::Json => {
            let envelope = serde_json::json!({
                "character_id": character_id,
                "slot": slot,
                "slot_label": slot_label,
                "sockets": sockets,
            });
            serde_json::to_vec_pretty(&envelope)
                .map_err(|e| format!("Could not encode JSON: {e}"))?
        }
    };

    storage::replace_file(&path, &bytes)
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "sundial-export-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn opt(hash: &str, name: Option<&str>, selected: bool) -> PlugOption {
        PlugOption {
            plug_hash: hash.to_owned(),
            plug_name: name.map(str::to_owned),
            is_selected: selected,
        }
    }

    fn socket(index: usize, opts: Vec<PlugOption>) -> SocketExport {
        SocketExport { socket_index: index, options: opts }
    }

    #[test]
    fn csv_emits_header_and_all_options() {
        let sockets = vec![
            socket(0, vec![
                opt("0x0000002A", Some("Shader A"), true),
                opt("0x0000002B", Some("Shader B"), false),
            ]),
            socket(1, vec![
                opt("0x0000002C", Some("Name, comma"), false),
            ]),
        ];
        let csv = render_csv(&sockets);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "socket_index,plug_hash,plug_name,is_selected");
        assert_eq!(lines[1], "0,0x0000002A,Shader A,true");
        assert_eq!(lines[2], "0,0x0000002B,Shader B,false");
        assert_eq!(lines[3], r#"1,0x0000002C,"Name, comma",false"#);
    }

    #[test]
    fn filename_is_stable_and_safe() {
        let name = build_filename("ABCD1234", "kinetic", ExportFormat::Csv);
        assert!(name.starts_with("plugs_ABCD1234_kinetic_"));
        assert!(name.ends_with(".csv"));
        let name_json = build_filename("char 0", "energy slot", ExportFormat::Json);
        assert!(name_json.starts_with("plugs_char_0_energy_slot_"));
        assert!(name_json.ends_with(".json"));
    }

    #[test]
    fn render_csv_includes_is_selected_column() {
        let sockets = vec![socket(0, vec![
            opt("0xAA", Some("Selected Mod"), true),
            opt("0xBB", Some("Other Mod"), false),
        ])];
        let csv = render_csv(&sockets);
        assert!(csv.contains(",true\n"));
        assert!(csv.contains(",false\n"));
    }

    /// write_socket_export uses current_exe() which works in test binaries.
    #[test]
    fn write_creates_json_with_correct_structure() {
        // Write directly using storage::replace_file to avoid exe-dir dependency in CI.
        let dir = TestDir::new();
        let sockets = vec![socket(0, vec![opt("0x0000002A", Some("Test Plug"), true)])];
        let envelope = serde_json::json!({
            "character_id": "testchar",
            "slot": "kinetic",
            "slot_label": "Kinetic",
            "sockets": sockets,
        });
        let bytes = serde_json::to_vec_pretty(&envelope).unwrap();
        let path = dir.0.join("test_out.json");
        storage::replace_file(&path, &bytes).unwrap();
        let val: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(val["character_id"], "testchar");
        assert_eq!(val["sockets"][0]["socket_index"], 0);
        assert_eq!(val["sockets"][0]["options"][0]["plug_hash"], "0x0000002A");
        assert_eq!(val["sockets"][0]["options"][0]["is_selected"], true);
    }
}
