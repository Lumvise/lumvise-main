//! Release CLI arguments. Source ownership stays explicit across Cargo workspaces.

use lumvise_builtin_plugin_release::BuiltinCompilationProfile;
use lumvise_plugin_package::ReleaseComposition;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

pub(crate) struct ReleaseOptions {
    pub(crate) profile: BuiltinCompilationProfile,
    pub(crate) workspaces: Vec<PathBuf>,
    pub(crate) target_dir: PathBuf,
    pub(crate) composition: ReleaseComposition,
    pub(crate) key: PathBuf,
    pub(crate) output: PathBuf,
    pub(crate) community_output: Option<PathBuf>,
    pub(crate) targets: Vec<String>,
}

impl ReleaseOptions {
    pub(crate) fn parse(arguments: Vec<String>, current: &Path) -> Result<Self, String> {
        let mut values = VecDeque::from(arguments);
        let mut profile = BuiltinCompilationProfile::Release;
        let mut workspaces = Vec::new();
        let mut target_dir = None;
        let mut community_output = None;
        while let Some(flag) = values.front().map(String::as_str) {
            match flag {
                "--debug" => {
                    profile = BuiltinCompilationProfile::Debug;
                    values.pop_front();
                }
                "--workspace" => workspaces.push(take_path(&mut values, current)?),
                "--target-dir" => target_dir = Some(take_path(&mut values, current)?),
                "--community-output" => community_output = Some(take_path(&mut values, current)?),
                _ => break,
            }
        }
        if workspaces.is_empty() {
            workspaces.push(current.to_path_buf());
        }
        let target_dir = target_dir.unwrap_or_else(|| default_target_dir(current, profile));
        let mut options = Self::from_positional(values.into(), profile, workspaces, target_dir)?;
        if let Some(path) = community_output {
            if options.composition != ReleaseComposition::Full
                || path == current.join(&options.output)
            {
                return Err(format!(
                    "community output `{}`; expected a distinct directory alongside a full composition",
                    path.display()
                ));
            }
            options.community_output = Some(path);
        }
        Ok(options)
    }

    fn from_positional(
        values: Vec<String>,
        profile: BuiltinCompilationProfile,
        workspaces: Vec<PathBuf>,
        target_dir: PathBuf,
    ) -> Result<Self, String> {
        let [flag, composition, key, output, targets @ ..] = values.as_slice() else {
            return Err(format!("arguments {values:?}; expected {}", usage()));
        };
        if flag != "--composition" || targets.is_empty() {
            return Err(format!("arguments {values:?}; expected {}", usage()));
        }
        let composition = match composition.as_str() {
            "minimal" => ReleaseComposition::Minimal,
            "full" => ReleaseComposition::Full,
            value => return Err(format!("composition `{value}`; expected minimal or full")),
        };
        Ok(Self {
            profile,
            workspaces,
            target_dir,
            composition,
            key: key.into(),
            output: output.into(),
            community_output: None,
            targets: targets.to_vec(),
        })
    }
}

fn take_path(values: &mut VecDeque<String>, current: &Path) -> Result<PathBuf, String> {
    let flag = values.pop_front().expect("matched path option");
    let value = values
        .pop_front()
        .filter(|value| !value.is_empty() && !value.starts_with("--"))
        .ok_or_else(|| format!("option `{flag}`; expected a following filesystem path"))?;
    Ok(current.join(value))
}

fn default_target_dir(current: &Path, profile: BuiltinCompilationProfile) -> PathBuf {
    current.join(match profile {
        BuiltinCompilationProfile::Debug => "target/builtin-plugin-debug",
        BuiltinCompilationProfile::Release => "target/builtin-plugin-release",
    })
}

fn usage() -> &'static str {
    "lumvise-builtin-plugin-release [--debug] [--workspace <root> ...] [--target-dir <path>] [--community-output <path>] --composition <minimal|full> <protected-signing-key> <output-dir> <target> [target ...]"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(prefix: &[&str]) -> Vec<String> {
        prefix
            .iter()
            .chain(
                [
                    "--composition",
                    "minimal",
                    "key",
                    "output",
                    "aarch64-apple-darwin",
                ]
                .iter(),
            )
            .map(|value| (*value).to_string())
            .collect()
    }

    #[test]
    fn defaults_to_one_current_workspace_and_release_profile() {
        let options = ReleaseOptions::parse(arguments(&[]), Path::new("/public")).unwrap();
        assert_eq!(options.workspaces, [PathBuf::from("/public")]);
        assert_eq!(options.profile, BuiltinCompilationProfile::Release);
        assert_eq!(
            options.target_dir,
            Path::new("/public/target/builtin-plugin-release")
        );
        assert_eq!(options.composition, ReleaseComposition::Minimal);
    }

    #[test]
    fn resolves_explicit_source_workspaces_and_build_cache() {
        let options = ReleaseOptions::parse(
            arguments(&[
                "--workspace",
                "core",
                "--debug",
                "--workspace",
                "/private",
                "--target-dir",
                "target/shared",
            ]),
            Path::new("/parent"),
        )
        .unwrap();
        assert_eq!(
            options.workspaces,
            [PathBuf::from("/parent/core"), PathBuf::from("/private")]
        );
        assert_eq!(options.target_dir, Path::new("/parent/target/shared"));
        assert_eq!(options.profile, BuiltinCompilationProfile::Debug);
    }

    #[test]
    fn malformed_source_options_name_the_offending_value() {
        let error = ReleaseOptions::parse(arguments(&["--workspace"]), Path::new("/public"))
            .err()
            .unwrap();
        assert!(error.contains("--workspace"));
        assert!(error.contains("expected a following filesystem path"));
        let error = ReleaseOptions::parse(vec!["unexpected".into()], Path::new("/public"))
            .err()
            .unwrap();
        assert!(error.contains("unexpected"));
    }

    #[test]
    fn shared_community_output_requires_full_build_and_separate_destination() {
        let mut values = arguments(&["--community-output", "community"]);
        assert!(ReleaseOptions::parse(values.clone(), Path::new("/root")).is_err());
        values[3] = "full".into();
        let options = ReleaseOptions::parse(values.clone(), Path::new("/root")).unwrap();
        assert_eq!(
            options.community_output,
            Some(PathBuf::from("/root/community"))
        );
        values[1] = "output".into();
        assert!(ReleaseOptions::parse(values, Path::new("/root")).is_err());
    }
}
