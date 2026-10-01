//! Retains release symbolication data before stripping staged macOS payloads.
//! Cargo's original executables remain intact for repeatable cached builds.

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub(crate) struct NativeSymbols {
    directory: PathBuf,
    uuid: String,
}

impl NativeSymbols {
    pub(crate) fn capture(binary: &Path, diagnostics: &Path) -> Result<Self, String> {
        fs::create_dir_all(diagnostics)
            .map_err(|error| format!("{}: {error}", diagnostics.display()))?;
        let symbols = diagnostics.join("symbols.dSYM");
        let output = checked(
            Command::new("xcrun")
                .arg("dsymutil")
                .arg(binary)
                .arg("-o")
                .arg(&symbols),
        )?;
        if String::from_utf8_lossy(&output.stderr).contains("no debug symbols") {
            return Err(format!(
                "{}; expected release debug information for private symbolication",
                binary.display()
            ));
        }
        let uuid = executable_uuid(binary)?;
        if executable_uuid(&symbols)? != uuid {
            return Err(format!(
                "{}; expected dSYM UUID matching {uuid}",
                symbols.display()
            ));
        }
        verify_source_mapping(&symbols, diagnostics)?;
        checked(
            Command::new("xcrun")
                .args(["strip", "-S", "-x"])
                .arg(binary),
        )?;
        Ok(Self {
            directory: diagnostics.into(),
            uuid,
        })
    }

    pub(crate) fn record_signed_payload(&self, binary: &Path) -> Result<(), String> {
        let record = serde_json::json!({
            "binary":binary.file_name().and_then(|name| name.to_str()),
            "uuid":self.uuid, "sha256":binary_sha256(binary)?, "symbols":"symbols.dSYM",
        });
        let manifest = self.directory.join("manifest.json");
        let bytes = serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?;
        fs::write(&manifest, bytes).map_err(|error| format!("{}: {error}", manifest.display()))
    }
}

fn verify_source_mapping(symbols: &Path, diagnostics: &Path) -> Result<(), String> {
    // Optimized code can share address ranges (LLVM #117952); prove the
    // address-to-source lookup needed for crash symbolication directly.
    let lines = checked(
        Command::new("xcrun")
            .args(["dwarfdump", "--debug-line"])
            .arg(symbols),
    )?;
    let address = source_address(&String::from_utf8_lossy(&lines.stdout))?;
    let lookup = checked(
        Command::new("xcrun")
            .args(["dwarfdump", &format!("--lookup={address}")])
            .arg(symbols),
    )?;
    if !String::from_utf8_lossy(&lookup.stdout).contains("Line info: file '") {
        return Err(format!(
            "{} address {address}; expected source file and line lookup",
            symbols.display()
        ));
    }
    fs::write(diagnostics.join("source-lookup.txt"), lookup.stdout)
        .map_err(|error| format!("{}: {error}", diagnostics.display()))
}

fn source_address(lines: &str) -> Result<String, String> {
    lines
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let address = fields.next()?.strip_prefix("0x")?;
            let source_line = fields.next()?.parse::<u64>().ok()?;
            (source_line > 0 && u64::from_str_radix(address, 16).is_ok())
                .then(|| format!("0x{address}"))
        })
        .ok_or_else(|| {
            "DWARF line table without source addresses; expected nonzero source line".into()
        })
}

fn executable_uuid(binary: &Path) -> Result<String, String> {
    let output = checked(
        Command::new("xcrun")
            .args(["dwarfdump", "--uuid"])
            .arg(binary),
    )?;
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .nth(1)
        .map(str::to_owned)
        .filter(|uuid| uuid.len() == 36)
        .ok_or_else(|| {
            format!(
                "{}; expected one Mach-O UUID from dwarfdump",
                binary.display()
            )
        })
}

fn binary_sha256(binary: &Path) -> Result<String, String> {
    let mut source = fs::File::open(binary).map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Ok(hex::encode(digest.finalize()));
        }
        digest.update(&buffer[..count]);
    }
}

fn checked(command: &mut Command) -> Result<Output, String> {
    let output = command
        .output()
        .map_err(|error| format!("{command:?}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{command:?} returned {}; expected successful native release preparation: {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use lumvise_builtin_plugin_release::BuiltinReleaseMatrix;
    use std::collections::BTreeMap;

    #[test]
    fn source_lookup_requires_a_real_address_and_nonzero_line() {
        assert_eq!(
            source_address("header\n0x1000 0\n0x1010 42 7\n").unwrap(),
            "0x1010"
        );
        for invalid in ["", "0x1000 0", "0xnope 42", "file_names[1] = foo.rs"] {
            assert!(
                source_address(invalid)
                    .unwrap_err()
                    .contains("expected nonzero source line")
            );
        }
    }

    #[test]
    fn retains_matching_symbols_and_records_only_the_stripped_signed_copy() {
        let temporary = tempfile::tempdir().unwrap();
        let original = std::env::current_exe().unwrap();
        let original_hash = binary_sha256(&original).unwrap();
        let matrix = BuiltinReleaseMatrix {
            descriptors: BTreeMap::new(),
            manifests: BTreeMap::new(),
            targets: BTreeMap::from([(
                "aarch64-apple-darwin".into(),
                BTreeMap::from([("fixture.plugin".into(), original.clone())]),
            )]),
        };
        let staged =
            super::super::stage_payloads(&matrix, &temporary.path().join("staging")).unwrap();
        let binary = &staged.targets["aarch64-apple-darwin"]["fixture.plugin"];
        let diagnostics = temporary.path().join("private");
        super::super::prepare_macos_plugins(&staged, Some("-"), Some(&diagnostics)).unwrap();
        let diagnostics = diagnostics.join("native/plugins/aarch64-apple-darwin/fixture.plugin");
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(diagnostics.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(record["sha256"], binary_sha256(&binary).unwrap());
        assert_eq!(record["uuid"], executable_uuid(&binary).unwrap());
        assert_eq!(original_hash, binary_sha256(&original).unwrap());
        assert!(diagnostics.join("symbols.dSYM").is_dir());
        assert!(
            fs::read_to_string(diagnostics.join("source-lookup.txt"))
                .unwrap()
                .contains("Line info: file '")
        );
        let load_commands = checked(Command::new("otool").arg("-l").arg(&binary)).unwrap();
        assert!(!String::from_utf8_lossy(&load_commands.stdout).contains("__DWARF"));
    }
}
